// Overclock NPC personas — seeded demo bots (casual only, never ranked).
// Uses NEAR AI via env keys. Keys are NEVER sent to the browser or contract.
// Usage:
//   NPC_ACCOUNT=npc-pro.testnet NPC_KEY=ed25519:... CONTRACT=overclockgame.testnet \
//     NEAR_AI_API_KEY=... node agents/run.mjs --persona pro [--tier 2]
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { connect, keyStores, KeyPair, providers } from "near-api-js";

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

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const retryable = (e) => !/(FunctionCallError|unreachable)/i.test(String(e?.message ?? e) + String(e?.cause ?? ""));

// Dual-lane RPC: rotate public testnet endpoints for every view. Free-tier
// rate limits on a single endpoint are what previously made NPCs miss the
// 30s commit windows; rotating spreads the load cheaply.
const RPC_ENDPOINTS = [
  "https://test.rpc.fastnear.com",
  "https://rpc.testnet.near.org",
  "https://rpc.testnet.fastnear.com",
];
let rpcCursor = 0;
const jsonView = async (url, method, args) => {
  const res = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: "1",
      method: "query",
      params: {
        request_type: "call_function",
        account_id: CONTRACT,
        method_name: method,
        args_base64: Buffer.from(JSON.stringify(args)).toString("base64"),
        finality: "optimistic",
      },
    }),
  });
  const body = await res.json();
  if (body.error) throw new Error(`rpc ${url}: ${JSON.stringify(body.error).slice(0, 160)}`);
  return JSON.parse(Buffer.from(body.result.result).toString()) ?? null;
};
const view = async (m, a = {}) => {
  for (let attempt = 0; ; attempt++) {
    const url = RPC_ENDPOINTS[(rpcCursor++) % RPC_ENDPOINTS.length];
    try {
      return await jsonView(url, m, a);
    } catch (e) {
      if (attempt >= 4) throw e;
      if (retryable(e)) await sleep(300 * 2 ** attempt);
      else throw e;
    }
  }
};
const call = (method, args, deposit = "0", gas = 50_000_000_000_000n) =>
  account.functionCall({ contractId: CONTRACT, methodName: method, args, gas, attachedDeposit: BigInt(deposit) });
const callTolerant = async (method, args, deposit = "0", gas = 50_000_000_000_000n) => {
  // Transport-level retries only (429 rate limit). Execution errors (contract
  // traps) are rethrown — a trap means the tx already landed and won't change.
  for (let i = 0; ; i++) {
    try {
      return await call(method, args, deposit, gas);
    } catch (e) {
      const s = JSON.stringify(e) + String(e?.message ?? "");
      const stuck = /FunctionCallError|unreachable/.test(s);
      if (stuck || i >= 4) throw e;
      await sleep(4000 * 2 ** i);
    }
  }
};
// ---------------------------------------------------------------------------
// Persistent duel loop — replaces the old single-duel run:
//   scan → recover/play active duel → (re)queue → repeat forever.
// A crash mid-duel no longer strands the duel: on restart we play or settle
// the active duel BEFORE doing any queue bookkeeping.
// ---------------------------------------------------------------------------
const STATE_FILE = path.join(path.dirname(path.resolve(".")), `.overclock-npc-state-${NPC_ACCOUNT}.json`);
const ROUNDS = process.env.NPC_ROUNDS ? Number(process.env.NPC_ROUNDS) : Infinity;

let duelState = {};
try { duelState = JSON.parse(fs.readFileSync(STATE_FILE, "utf8")); } catch { /* fresh */ }
const saveState = () => { try { fs.writeFileSync(STATE_FILE, JSON.stringify(duelState)); } catch { /* best effort */ } };

let lastSeenSeq = -1; // enumerate duels only when duel_seq moves — 1 RPC per idle cycle
const scanMyDuels = async () => {
  const cfg = await view("get_config");
  const seq = Number(cfg.duel_seq);
  if (seq === lastSeenSeq) return [];
  lastSeenSeq = seq;
  const mine = [];
  // Our active duel is almost always among the newest few — newer duels are
  // only created after ours settled. 5 duels keep scans cheap (no 429 storms).
  for (let id = seq; id > 0 && id > seq - 5; id--) {
    try {
      const d = await view("get_duel", { duel_id: id });
      if (d && (d.a === NPC_ACCOUNT || d.b === NPC_ACCOUNT)) mine.push(d);
    } catch { /* pruned */ }
  }
  return mine;
};

const playDuel = async (first) => {
  const id = Number(first.id);
  const isA = first.a === NPC_ACCOUNT;
  const committedAt = isA !== false ? {} : {};
  let d = await view("get_duel", { duel_id: id });
  const committedBefore = !!(isA ? d.hash_a : d.hash_b);
  const revealedBefore = isA ? d.revealed_a : d.revealed_b;
  console.log(`[npc:${persona}] duel #${id} vs ${isA ? d.b : d.a} — committed=${committedBefore} revealed_a=${d.revealed_a} revealed_b=${d.revealed_b}`);

  // Solve once and persist the exact answer: commit and reveal must match even
  // across watchdog restarts (a fresh AI answer could otherwise break reveal).
  let answer = duelState[id]?.answer ?? null;
  if (answer !== null) {
    console.log(`[npc:${persona}] duel #${id}: using persisted answer ${answer}`);
  } else {
    const problem = await view("get_problem", { duel_id: id });
    answer = solveText(problem.text);
    try {
      const ai = await askModel(problem.text);
      if (ai !== null) {
        console.log(`[npc:${persona}] model answered ${ai} (local ${answer})`);
        answer = ai;
      }
    } catch (e) {
      console.log(`[npc:${persona}] model failed (${e.message}) — answering locally: ${answer}`);
    }
    duelState[id] = { answer: String(answer) };
    saveState();
  }

  let committedHere = false;

  // 1) Commit while the window is open and we have not committed yet.
  if (!d.settled && !committedBefore && Date.now() < Number(d.deadline)) {
    const salt = `salt-${id}-${NPC_ACCOUNT}`;
    const commitHash = sha256hex(`${id}:${NPC_ACCOUNT}:${answer}:${salt}`);
    console.log(`[npc:${persona}] duel #${id}: committing ${answer}…`);
    try {
      await callTolerant("submit_answer", { duel_id: id, answer_hash: commitHash });
      console.log(`[npc:${persona}] duel #${id}: committed`);
      committedHere = true;
    } catch (e) {
      // Window may have closed between the check and our tx landing — settle path below handles it.
      console.log(`[npc:${persona}] duel #${id}: commit rejected — ${String(e?.message ?? e).slice(0, 120)}`);
    }
  } else if (committedBefore) {
    console.log(`[npc:${persona}] duel #${id}: already committed from an earlier session`);
  } else if (!d.settled) {
    const lateSec = Math.round((Date.now() - Number(d.deadline)) / 1000);
    console.log(`[npc:${persona}] duel #${id}: discovered ${lateSec}s too late for commit — settling`);
  }

  // 2) Reveal once the contract allows it (both committed, or deadline passed).
  let revealAnswerable = committedHere || duelState[id]?.answer === String(answer);
  let revealed = revealedBefore;
  while (!revealed && Date.now() < Number(d.reveal_deadline) && revealAnswerable) {
    d = await view("get_duel", { duel_id: id });
    if (d.settled) break;
    revealed = isA ? d.revealed_a : d.revealed_b;
    if (revealed) break;
    const both = !!(d.hash_a && d.hash_b);
    const commitWindowOver = Date.now() > Number(d.deadline);
    if (!both && !commitWindowOver) {
      await sleep(1500);
      continue;
    }
    try {
      await callTolerant("reveal", { duel_id: id, answer: String(answer), salt: `salt-${id}-${NPC_ACCOUNT}` });
      console.log(`[npc:${persona}] duel #${id}: revealed`);
      revealed = true;
    } catch (e) {
      console.log(`[npc:${persona}] duel #${id}: reveal pending — ${String(e?.message ?? e).slice(0, 120)}`);
      await sleep(3000);
    }
  }
  if (!revealed && !d.settled) {
    console.log(`[npc:${persona}] duel #${id}: not revealing (committed=${committedBefore}/${revealAnswerable}) — settling once the window closes`);
  }

  // 3) Settle as soon as the contract allows; keep trying within the window.
  for (let i = 0; i < 30; i++) {
    try {
      d = await view("get_duel", { duel_id: id });
      if (d.settled) {
        console.log(`[npc:${persona}] duel #${id} settled: winner=${d.winner} draw=${d.draw}`);
        return;
      }
      await callTolerant("settle", { duel_id: id }, "0", 200_000_000_000_000n);
      const after = await view("get_duel", { duel_id: id });
      console.log(`[npc:${persona}] duel #${id} settled: winner=${after.winner} draw=${after.draw}`);
      return;
    } catch (e) {
      await sleep(4000);
    }
  }
  console.log(`[npc:${persona}] duel #${id}: settle window not ready — will retry on next loop`);
};

let rounds = 0;
console.log(`[npc:${persona}] persistent duel loop starting (tier ${tier})`);
while (rounds < ROUNDS) {
  try {
    const mine = await scanMyDuels();
    const active = mine.filter((x) => !x.settled);
    if (active.length) {
      // In-flight duel takes priority over any queue bookkeeping — commit
      // windows are short and every repeat cycle is a lost duel.
      await playDuel(active[active.length - 1]);
      continue;
    }
    // No active duel. Make sure we are queued (idempotently, minimal txs).
    const p = await view("get_player", { account: NPC_ACCOUNT }).catch(() => null);
    if (!p) throw new Error("player not registered yet");
    if (p.queued) {
      if (rounds % 24 === 0) console.log(`[npc:${persona}] queued — waiting for a match…`);
    } else {
      try {
        await callTolerant("join_queue", { tier, casual: true }, tierStake);
        console.log(`[npc:${persona}] joined tier ${tier} (casual)`);
      } catch (e) {
        const p2 = await view("get_player", { account: NPC_ACCOUNT }).catch(() => null);
        if (!p2?.queued) throw e;
        console.log(`[npc:${persona}] already queued — adopting it`);
      }
    }
  } catch (e) {
    console.log(`[npc:${persona}] cycle error: ${String(e?.message ?? e).slice(0, 200)}`);
  }
  rounds++;
  await sleep(2000); // 2s cadence — a human join creates a duel instantly and the commit window is only 30s
}
