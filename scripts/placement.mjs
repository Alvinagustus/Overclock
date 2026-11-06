// Overclock placement runner — contract-scored accreditation (v2).
// The contract draws the 20 problems and scores them; this script computes
// the answers locally (or via NEAR AI) and submits them.
// Usage:
//   ACCREDIT_ACCOUNT=owner.testnet ACCREDIT_KEY=ed25519:... CONTRACT=overclockgame.testnet \
//     [NEAR_AI_API_KEY=...] node scripts/placement.mjs [--submit]
import { connect, keyStores, KeyPair } from "near-api-js";

const STORAGE_DEPOSIT = "5120000000000000000000"; // 0.00512 NEAR, refundable (512 bytes)
const doSubmit = process.argv.includes("--submit");
const CONTRACT = process.env.CONTRACT || "overclockgame.testnet";
const { ACCREDIT_ACCOUNT, NEAR_AI_API_KEY } = process.env;
const ACCREDIT_KEY_RAW = process.env.ACCREDIT_KEY;
// Accept a raw secret as well as the "ed25519:..." prefixed form.
const ACCREDIT_KEY = ACCREDIT_KEY_RAW && !ACCREDIT_KEY_RAW.includes(":") ? `ed25519:${ACCREDIT_KEY_RAW}` : ACCREDIT_KEY_RAW;
if (doSubmit && (!ACCREDIT_ACCOUNT || !ACCREDIT_KEY)) {
  throw new Error("ACCREDIT_ACCOUNT and ACCREDIT_KEY are required with --submit");
}

function gcd(a, b) { a = Math.abs(a); b = Math.abs(b); while (b) { [a, b] = [b, a % b]; } return a; }
function powMod(base, exp, m) {
  let r = 1n, b = ((BigInt(base) % BigInt(m)) + BigInt(m)) % BigInt(m), e = BigInt(exp);
  const mm = BigInt(m);
  while (e > 0n) { if (e & 1n) r = r * b % mm; b = b * b % mm; e >>= 1n; }
  return Number(r);
}
function solveText(t) {
  const s = t.trim();
  let m;
  if ((m = s.match(/^(-?\d+) \+ (-?\d+) - (-?\d+) = \?$/))) return Number(m[1]) + Number(m[2]) - Number(m[3]);
  if ((m = s.match(/^(-?\d+) \* (-?\d+) = \?$/))) return Number(m[1]) * Number(m[2]);
  if ((m = s.match(/^(-?\d+) mod (-?\d+) = \?$/))) return Number(m[1]) % Number(m[2]);
  if ((m = s.match(/^\((-?\d+) \* (-?\d+)\) mod (-?\d+) = \?$/))) return Number((BigInt(m[1]) * BigInt(m[2])) % BigInt(m[3]));
  if ((m = s.match(/^gcd\((-?\d+), (-?\d+)\) = \?$/))) return gcd(Number(m[1]), Number(m[2]));
  if ((m = s.match(/^(-?\d+)\^(-?\d+) mod (-?\d+) = \?$/))) return powMod(Number(m[1]), Number(m[2]), Number(m[3]));
  if ((m = s.match(/^x = \? such that (-?\d+) \* x = 1 \(mod (-?\d+)\), 0 < x < \2$/))) {
    const a = BigInt(m[1]), c = BigInt(m[2]);
    let [old_r, r] = [a, c], [old_s, s2] = [1n, 0n];
    while (r !== 0n) { const q = old_r / r; [old_r, r] = [r, old_r - q * r]; [old_s, s2] = [s2, old_s - q * s2]; }
    if (old_r !== 1n) return NaN;
    return Number(((old_s % c) + c) % c);
  }
  if ((m = s.match(/^popcount\((-?\d+) XOR (-?\d+)\) = \?$/))) {
    return ((BigInt(m[1]) ^ BigInt(m[2])).toString(2).match(/1/g) || []).length;
  }
  return NaN;
}

async function askModel(problemText) {
  if (!NEAR_AI_API_KEY) return null;
  const res = await fetch("https://cloud-api.near.ai/v1/chat/completions", {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${NEAR_AI_API_KEY}` },
    body: JSON.stringify({
      model: process.env.MODEL_PLACEMENT || "z-ai/glm-5.3-flash",
      messages: [
        { role: "system", content: "You solve compute problems. Reply with ONLY the integer answer." },
        { role: "user", content: problemText },
      ],
      temperature: 0,
      max_tokens: 100,
    }),
  });
  if (!res.ok) throw new Error(`NEAR AI ${res.status}`);
  const j = await res.json();
  const raw = (j.choices?.[0]?.message?.content ?? "").trim();
  const parsed = Number(raw.replace(/[^\d-]/g, ""));
  return Number.isFinite(parsed) ? parsed : null;
}

const keyStore = new keyStores.InMemoryKeyStore();
if (doSubmit) await keyStore.setKey("testnet", ACCREDIT_ACCOUNT, KeyPair.fromString(ACCREDIT_KEY));
const near = await connect({
  networkId: "testnet",
  keyStore,
  nodeUrl: "https://test.rpc.fastnear.com",
});
const account = doSubmit ? await near.account(ACCREDIT_ACCOUNT) : await near.account(CONTRACT);
const view = (m, a = {}) => account.viewFunction({ contractId: CONTRACT, methodName: m, args: a });

// Start the placement on-chain (needs the caller's wallet for the tx).
// Both calls attach the refundable storage deposit: register_agent funds
// brand-new accounts, start_placement tops up already-registered but
// unfunded accounts (register_agent is idempotent and refunds there).
let problems;
if (doSubmit) {
  try {
    await account.functionCall({ contractId: CONTRACT, methodName: "register_agent", args: {}, gas: 50_000_000_000_000n, attachedDeposit: BigInt(STORAGE_DEPOSIT) });
  } catch { /* already registered */ }
  await account.functionCall({ contractId: CONTRACT, methodName: "start_placement", args: {}, gas: 50_000_000_000_000n, attachedDeposit: BigInt(STORAGE_DEPOSIT) });
  problems = await view("get_placement_problems", { account: ACCREDIT_ACCOUNT });
} else {
  // preview mode: show the problems of a running attempt if any
  problems = await view("get_placement_problems", { account: "preview.testnet" }).catch(() => []);
  console.log("preview mode: run with --submit and ACCREDIT_ACCOUNT/ACCREDIT_KEY to actually accredit.");
}

const answers = [];
for (const p of problems) {
  let a = solveText(p.text);
  try { const ai = await askModel(p.text); if (ai !== null) a = ai; } catch { /* fall back to local */ }
  answers.push(a);
  console.log(`${String(answers.length).padStart(2, " ")}. ${p.text} -> ${a}`);
}
console.log(`\nsolved ${answers.filter(Number.isFinite).length}/${problems.length} locally`);

if (doSubmit) {
  const res = await account.functionCall({
    contractId: CONTRACT,
    methodName: "submit_placement",
    args: { answers: answers.map((a) => Math.trunc(a || 0)) },
    gas: 80_000_000_000_000n,
  });
  console.log("submitted. tx:", res.transaction?.hash);
}
