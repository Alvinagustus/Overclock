// Overclock NPC personas — seeded demo bots (casual only, never ranked).
// Uses NEAR AI via env keys. Keys are NEVER sent to the browser or contract.
// Usage:
//   NPC_ACCOUNT=npc-pro.testnet NPC_KEY=ed25519:... CONTRACT=overclockgame.testnet \
//     NEAR_AI_API_KEY=... node agents/run.mjs --persona pro [--tier 2]
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { connect, keyStores, KeyPair } from "near-api-js";

const persona = process.argv.includes("--persona")
  ? process.argv[process.argv.indexOf("--persona") + 1]
  : "pro";
const tier = Number(process.argv.includes("--tier") ? process.argv[process.argv.indexOf("--tier") + 1] : 2);
const STORAGE_DEPOSIT = "5120000000000000000000"; // 0.00512 NEAR, refundable (512 bytes)
const TIER_STAKE = { 1: "10000000000000000000000", 2: "100000000000000000000000", 3: "1000000000000000000000000" };
const tierStake = TIER_STAKE[tier];

const stem = path.resolve(import.meta.dirname, "..");
const promptFile = path.join(stem, "agents", `prompt-${persona}.md`);
const systemPrompt = fs.existsSync(promptFile) ? fs.readFileSync(promptFile, "utf8") : "You are an Overclock NPC bot.";

const NPC_ACCOUNT = process.env.NPC_ACCOUNT;
const NPC_KEY_RAW = process.env.NPC_KEY;
const NPC_KEY = NPC_KEY_RAW.includes(":") ? NPC_KEY_RAW : `ed25519:${NPC_KEY_RAW}`;
const CONTRACT = process.env.CONTRACT || "overclockgame.testnet";
const NEAR_AI_KEY = process.env.NEAR_AI_API_KEY;
if (!NPC_ACCOUNT || !NPC_KEY) throw new Error("NPC_ACCOUNT and NPC_KEY env required");

const MODELS = {
  novice: process.env.MODEL_NOVICE || "z-ai/glm-5.3-flash",
  pro: process.env.MODEL_PRO || "deepseek/deepseek-v4.1-flash",
};

const sha256hex = (s) => crypto.createHash("sha256").update(s).digest("hex");

// ---- local solver for the 8 generated problem formats ----------------------
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
  if (!NEAR_AI_KEY) return null; // solve locally below
  const res = await fetch("https://cloud-api.near.ai/v1/chat/completions", {
    method: "POST",
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${NEAR_AI_KEY}` },
    body: JSON.stringify({
      model: MODELS[persona],
      messages: [
        { role: "system", content: systemPrompt },
        { role: "user", content: `Solve: ${problemText}\nReply with ONLY the integer answer.` },
      ],
      temperature: 0,
      max_tokens: 100,
    }),
  });
  if (!res.ok) throw new Error(`NEAR AI ${res.status}: ${await res.text()}`);
  const j = await res.json();
  const raw = (j.choices?.[0]?.message?.content ?? "").trim();
  const parsed = Number(raw.replace(/[^\d-]/g, ""));
  return Number.isFinite(parsed) ? parsed : null;
}

const keyStore = new keyStores.InMemoryKeyStore();
await keyStore.setKey("testnet", NPC_ACCOUNT, KeyPair.fromString(NPC_KEY));
const near = await connect({ networkId: "testnet", keyStore, nodeUrl: "https://test.rpc.fastnear.com" });
const account = await near.account(NPC_ACCOUNT);
const view = (m, a = {}) => account.viewFunction({ contractId: CONTRACT, methodName: m, args: a });
const call = (method, args, deposit = "0", gas = 50_000_000_000_000n) =>
  account.functionCall({ contractId: CONTRACT, methodName: method, args, gas, attachedDeposit: BigInt(deposit) });

console.log(`[npc:${persona}] ${NPC_ACCOUNT} joining tier ${tier} (casual)…`);
try { await call("register_agent", {}, STORAGE_DEPOSIT); console.log(`[npc:${persona}] registered`); } catch { /* already registered */ }
await call("join_queue", { tier, casual: true }, tierStake);
console.log(`[npc:${persona}] queued — polling for a match…`);

let duelId = null;
for (let i = 0; i < 60 && !duelId; i++) {
  await new Promise((r) => setTimeout(r, 2000));
  const cfg = await view("get_config");
  const seq = Number(cfg.duel_seq);
  // Scan backwards across recent duels: our duel may be older than the latest one
  // if another duel started after we queued. Skip settled duels so a stale
  // duel from an earlier session is never picked.
  for (let id = seq; id > 0 && id > seq - 20; id--) {
    try {
      const d = await view("get_duel", { duel_id: id });
      if (d && (d.a === NPC_ACCOUNT || d.b === NPC_ACCOUNT) && !d.settled) { duelId = id; break; }
    } catch { /* duel_id 0 or pruned */ }
  }
}
if (!duelId) throw new Error("no duel (queue empty — start a human casual join first)");

const problem = await view("get_problem", { duel_id: duelId });
const localAnswer = solveText(problem.text);
let answer = localAnswer;
try {
  const ai = await askModel(problem.text);
  if (ai !== null) {
    console.log(`[npc:${persona}] model answered ${ai} (local computed ${localAnswer}, expected contract check applies)`);
    answer = ai;
  }
} catch (e) {
  console.log(`[npc:${persona}] model failed (${e.message}) — answering locally: ${localAnswer}`);
}

if (!Number.isFinite(answer)) throw new Error("cannot solve problem locally");

const salt = `salt-${duelId}-${NPC_ACCOUNT}`;
const commitHash = sha256hex(`${duelId}:${NPC_ACCOUNT}:${answer}:${salt}`);
console.log(`[npc:${persona}] duel #${duelId}: ${problem.text} committing ${answer}`);
await call("submit_answer", { duel_id: duelId, answer_hash: commitHash });
console.log(`[npc:${persona}] committed`);

// Reveal is only allowed once both sides have committed, or after the
// commit window closes (guard H-01). Wait for that; revealing earlier
// fails on-chain and would expose the answer while the opponent is open.
for (let i = 0; i < 40; i++) {
  const d = await view("get_duel", { duel_id: duelId });
  const both = !!(d.hash_a && d.hash_b);
  const pastDeadline = Date.now() > Number(d.deadline);
  if (both || pastDeadline) break;
  await new Promise((r) => setTimeout(r, 1000));
}
await call("reveal", { duel_id: duelId, answer, salt });
console.log(`[npc:${persona}] revealed`);

// Settle is allowed once every committed side has revealed (early settle),
// otherwise after the reveal window. Poll instead of a single blind wait so
// an opponent who already settled is not retried, and a slow window is
// retried rather than forgotten.
let settled = false;
for (let i = 0; i < 15 && !settled; i++) {
  try {
    const d = await view("get_duel", { duel_id: duelId });
    if (d.settled) { console.log(`[npc:${persona}] duel #${duelId} settled: winner=${d.winner} draw=${d.draw}`); settled = true; break; }
    await call("settle", { duel_id: duelId }, "0", 200_000_000_000_000n);
    const after = await view("get_duel", { duel_id: duelId });
    console.log(`[npc:${persona}] duel #${duelId} settled: winner=${after.winner} draw=${after.draw}`);
    settled = true;
  } catch (e) {
    if (i === 14) console.log(`[npc:${persona}] settle not ready (${e.message.slice(0,150)}) — anyone can settle after the window`);
    else await new Promise((r) => setTimeout(r, 2000));
  }
}

