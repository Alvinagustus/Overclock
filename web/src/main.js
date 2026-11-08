// Overclock web UI — v3 (targets overclock-v2 contract API).
// Testnet only. Wallet: MyNearWallet + Meteor.
// Add ?demo to the URL to explore the UI with an in-memory mock (no wallet / contract needed).
import { Buffer } from "buffer";
import { connect, Contract, keyStores, utils } from "near-api-js";
import { setupWalletSelector } from "@near-wallet-selector/core";
import { setupModal } from "@near-wallet-selector/modal-ui";
import { setupMyNearWallet } from "@near-wallet-selector/my-near-wallet";
import { setupMeteorWallet } from "@near-wallet-selector/meteor-wallet";
import { actionCreators } from "@near-js/transactions";
import "@near-wallet-selector/modal-ui/styles.css";

if (!globalThis.Buffer) globalThis.Buffer = Buffer;

const DEMO = new URLSearchParams(location.search).has("demo");
const DEMO_ACCOUNT = "agent-7.testnet";
const CONTRACT = localStorage.getItem("OVERCLOCK_CONTRACT") || "overclockgame.testnet";
const TIER_STAKE = { 1: "0.01", 2: "0.1", 3: "1" };
const COMMIT_MS = 30_000;
const REVEAL_MS = 20_000;
const RAKE = 0.03;
const VIEW = [
  "get_player", "get_duel", "get_config", "get_leaderboard",
  "get_problem", "get_placement_problems", "get_season_top", "get_claimable",
];

let selector = null;
let modal = null;
let wallet = null;
let viewC = null;
let demo = null;

let mode = "casual";
let tier = 2;
let playerInfo = null;
let boardRows = [];
let profileFor = null;

let currentDuelId = null;
let currentProblem = null;
let currentSalt = null;
let lastSig = "";
let loadingProblem = false;
let tickInt = null;
let pollInt = null;
let queueInt = null;
let queueTick = null;
let watchId = null;

const nearAction = (method, args, gas = 50_000_000_000_000n, deposit = 0n) =>
  actionCreators.functionCall(method, Buffer.from(JSON.stringify(args)), gas, deposit);

const $ = (id) => document.getElementById(id);
const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const pill = (t, k = "") => `<span class="pill ${k}">${esc(t)}</span>`;
const nearOf = (yocto) => Number(yocto) / 1e24;
const fmtNear = (n, d = 4) => `${Number(n).toFixed(d).replace(/\.?0+$/, "") || "0"} NEAR`;
const fmtYocto = (yocto) => fmtNear(nearOf(yocto));
const errMsg = (e) => String(e?.message || e || "Unknown error").slice(0, 220);
const acctLink = (a) => `https://testnet.nearblocks.io/address/${encodeURIComponent(a)}`;
const txUrl = (h) => `https://testnet.nearblocks.io/txns/${encodeURIComponent(h)}`;

// ───────────────── toasts + busy helper ─────────────────
function toast(title, { detail = "", tx = "", kind = "info" } = {}) {
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.innerHTML = `<b>${esc(title)}</b>${detail ? `<span>${esc(detail)}</span>` : ""}${
    tx ? `<span><a href="${txUrl(tx)}" target="_blank" rel="noreferrer">View transaction ${esc(String(tx).slice(0, 10))}…</a></span>` : ""}`;
  $("toasts").appendChild(el);
  const ttl = kind === "bad" ? 9000 : 5500;
  setTimeout(() => { el.classList.add("out"); setTimeout(() => el.remove(), 300); }, ttl);
}
const txHash = (res) => res?.transaction?.hash || res?.transaction_outcome?.id || "";

async function busy(btn, fn) {
  if (btn) { btn.classList.add("is-busy"); btn.disabled = true; }
  try { return await fn(); }
  finally { if (btn?.isConnected) { btn.classList.remove("is-busy"); btn.disabled = false; } }
}

// ───────────────── local problem solver (8 generated formats) ─────────────────
function gcd(a, b) { a = Math.abs(a); b = Math.abs(b); while (b) { [a, b] = [b, a % b]; } return a; }
function powMod(base, exp, m) {
  let r = 1n, b = ((BigInt(base) % BigInt(m)) + BigInt(m)) % BigInt(m), e = BigInt(exp);
  const mm = BigInt(m);
  while (e > 0n) { if (e & 1n) r = r * b % mm; b = b * b % mm; e >>= 1n; }
  return Number(r);
}
function parseSolve(text) {
  const t = text.trim();
  let m;
  if ((m = t.match(/^(-?\d+) \+ (-?\d+) - (-?\d+) = \?$/))) return Number(m[1]) + Number(m[2]) - Number(m[3]);
  if ((m = t.match(/^(-?\d+) \* (-?\d+) = \?$/))) return Number(m[1]) * Number(m[2]);
  if ((m = t.match(/^(-?\d+) mod (-?\d+) = \?$/))) return Number(m[1]) % Number(m[2]);
  if ((m = t.match(/^\((-?\d+) \* (-?\d+)\) mod (-?\d+) = \?$/))) return Number((BigInt(m[1]) * BigInt(m[2])) % BigInt(m[3]));
  if ((m = t.match(/^gcd\((-?\d+), (-?\d+)\) = \?$/))) return gcd(Number(m[1]), Number(m[2]));
  if ((m = t.match(/^(-?\d+)\^(-?\d+) mod (-?\d+) = \?$/))) return powMod(Number(m[1]), Number(m[2]), Number(m[3]));
  if ((m = t.match(/^x = \? such that (-?\d+) \* x = 1 \(mod (-?\d+)\), 0 < x < \2$/))) {
    const a = BigInt(m[1]), c = BigInt(m[2]);
    let [old_r, r] = [a, c], [old_s, s] = [1n, 0n];
    while (r !== 0n) {
      const q = old_r / r;
      [old_r, r] = [r, old_r - q * r];
      [old_s, s] = [s, old_s - q * s];
    }
    if (old_r !== 1n) return NaN;
    return Number(((old_s % c) + c) % c);
  }
  if ((m = t.match(/^popcount\((-?\d+) XOR (-?\d+)\) = \?$/))) {
    const x = (BigInt(m[1]) ^ BigInt(m[2])).toString(2);
    return (x.match(/1/g) || []).length;
  }
  return NaN;
}

async function sha256hex(text) {
  const buf = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

// ───────────────── wallet ─────────────────
function currentAccount() {
  if (DEMO) return { accountId: DEMO_ACCOUNT, active: true };
  const accounts = selector?.store.getState().accounts || [];
  return accounts.find((a) => a.active) || accounts[0] || null;
}
const me = () => currentAccount()?.accountId || null;

function renderWho() {
  const acc = currentAccount();
  const btn = $("connect");
  if (acc) {
    btn.textContent = DEMO ? "Demo mode" : "Disconnect";
    btn.classList.add("is-connected");
    $("who").innerHTML = `<span class="dot live"></span><a href="${acctLink(acc.accountId)}" target="_blank" rel="noreferrer" title="${esc(acc.accountId)}">${esc(acc.accountId)}</a>`;
  } else {
    btn.textContent = "Connect wallet";
    btn.classList.remove("is-connected");
    $("who").innerHTML = `<span class="dot"></span>Read-only`;
  }
}

async function syncWallet() {
  if (DEMO) wallet = {};
  else wallet = selector?.isSignedIn() ? await selector.wallet().catch(() => null) : null;
  renderWho();
  renderCta();
  renderBoard();
  const acc = me();
  if (acc !== profileFor) {
    profileFor = acc;
    await loadProfile();
    if (acc && !currentDuelId && !queueInt) reconcileQueue();
  }
}

async function initWallet() {
  if (DEMO) {
    $("connect").addEventListener("click", () => toast("Demo mode", { detail: "Remove ?demo from the URL to use a real wallet." }));
    return syncWallet();
  }
  selector = await setupWalletSelector({
    network: "testnet",
    modules: [
      setupMyNearWallet({ walletUrl: "https://testnet.mynearwallet.com" }),
      setupMeteorWallet(),
    ],
  });
  modal = setupModal(selector, { contractId: CONTRACT });
  $("connect").addEventListener("click", async () => {
    if (selector.isSignedIn()) {
      $("connect").disabled = true;
      try { await (await selector.wallet()).signOut(); } catch (e) { console.error(e); }
      finally { $("connect").disabled = false; await syncWallet(); }
      return;
    }
    modal.show();
  });
  selector.on("signedIn", () => syncWallet());
  selector.on("signedOut", () => syncWallet());
  selector.on("accountsChanged", () => syncWallet());
  window.addEventListener("focus", () => syncWallet());
  document.addEventListener("visibilitychange", () => { if (!document.hidden) syncWallet(); });
  await syncWallet();
}

async function initView() {
  if (DEMO) {
    const { createDemo } = await import("./demo.js");
    demo = createDemo(DEMO_ACCOUNT);
    viewC = demo.viewC;
    return;
  }
  const nearConn = await connect({
    networkId: "testnet",
    keyStore: new keyStores.BrowserLocalStorageKeyStore(),
    nodeUrl: "https://test.rpc.fastnear.com",
  });
  viewC = new Contract(nearConn.connection, CONTRACT, { viewMethods: VIEW, changeMethods: [] });
}

async function call(method, args, deposit = "0", gas = 50_000_000_000_000n) {
  if (DEMO) return demo.call(method, args);
  if (!wallet || !currentAccount()) throw new Error("Connect a wallet first (MyNearWallet or Meteor).");
  return wallet.signAndSendTransaction({
    signerId: currentAccount().accountId,
    receiverId: CONTRACT,
    actions: [nearAction(method, args, gas, BigInt(deposit))],
  });
}
function needWallet() {
  if (me()) return false;
  toast("Connect your wallet first", { detail: "MyNearWallet or Meteor.", kind: "bad" });
  modal?.show();
  return true;
}

// ───────────────── lobby (mode / tier / join button) ─────────────────
const MODE_NOTES = {
  casual: () => `<b>Casual</b> — practice against NPC Pro or other humans. Results never touch your ELO and don't count toward the hourly cap.`,
  ranked: () => {
    const accredited = Number(playerInfo?.elo) > 0;
    return accredited
      ? `<b>Ranked</b> — paired by ELO (±150 growing to ±450 while you wait). Max 10 ranked duels per hour.`
      : `<b>Ranked needs accreditation.</b> Pass the 20-problem test first — <a href="#profile-panel">go to “Your agent”</a>.`;
  },
};
function renderModeNote() {
  const n = $("mode-note");
  n.innerHTML = MODE_NOTES[mode]();
  n.classList.toggle("warn", mode === "ranked" && !(Number(playerInfo?.elo) > 0));
}
function renderCta() {
  const b = $("join");
  if (!b) return;
  b.textContent = me() ? `Find ${mode} match · ${TIER_STAKE[tier]} NEAR` : "Connect wallet to play";
  renderModeNote();
}
function bindSegmented(containerId, attr, onPick) {
  $(containerId).addEventListener("click", (e) => {
    const btn = e.target.closest(`button[data-${attr}]`);
    if (!btn) return;
    $(containerId).querySelectorAll("button").forEach((x) => {
      const on = x === btn;
      x.classList.toggle("on", on);
      x.setAttribute("aria-checked", String(on));
    });
    onPick(btn.dataset[attr]);
    renderCta();
  });
}
bindSegmented("mode", "mode", (v) => { mode = v; });
bindSegmented("tiers", "tier", (v) => { tier = Number(v); });

function enterDuelView(html) {
  $("lobby").hidden = true;
  const box = $("duel-box");
  box.hidden = false;
  box.innerHTML = html;
  $("arena").scrollIntoView({ block: "nearest" });
}
function leaveToLobby() {
  clearInterval(tickInt); clearInterval(pollInt); clearInterval(queueInt); clearInterval(queueTick);
  currentDuelId = null; currentProblem = null; currentSalt = null; lastSig = "";
  $("duel-box").hidden = true;
  $("duel-box").innerHTML = "";
  $("lobby").hidden = false;
  renderCta();
}

// Hide the queue UI without canceling — keeps polling + localStorage so a
// match still opens the duel and refresh/reconcile can restore status.
function minimizeQueue() {
  $("duel-box").hidden = true;
  $("lobby").hidden = false;
  toast("Still searching in the background", { detail: "Your queue is active on-chain. You'll jump into the duel when matched, or use the Arena to check status.", kind: "ok" });
  renderCta();
}

// ───────────────── queue + matching ─────────────────
async function scanForMyDuel(fromSeq, floor = 0) {
  const who = me();
  if (!who) return null;
  for (let id = fromSeq; id > floor && id > fromSeq - 20; id--) {
    const d = await viewC.get_duel({ duel_id: id }).catch(() => null);
    if (d && (d.a === who || d.b === who) && !d.settled) return id;
  }
  return null;
}

function queueView(title, sub, withCancel = true) {
  enterDuelView(`
    <div class="queue">
      <div class="radar" aria-hidden="true"><i></i></div>
      <div>
        <h3>${esc(title)}</h3>
        <p class="muted" id="q-sub">${sub}</p>
      </div>
      ${withCancel ? `<div class="actions" style="width:100%;max-width:340px">
        <button id="cancel-queue" class="btn btn-danger" type="button">Leave queue &amp; refund</button>
        <button id="queue-back" class="btn btn-ghost" type="button">Hide (keep searching)</button>
      </div>` : ""}
    </div>`);
  $("cancel-queue")?.addEventListener("click", cancelQueue);
  $("queue-back")?.addEventListener("click", minimizeQueue);
}

function startMatchPolling(startedAt) {
  clearInterval(queueInt); clearInterval(queueTick);
  queueTick = setInterval(() => {
    const el = $("q-elapsed");
    if (el) el.textContent = `${Math.floor((Date.now() - startedAt) / 1000)}s`;
  }, 1000);
  let lastSeq = null;
  const check = async () => {
    try {
      const cfg = await viewC.get_config();
      const seq = Number(cfg.duel_seq);
      if (lastSeq === null || seq !== lastSeq) {
        const found = await scanForMyDuel(seq, lastSeq ?? 0);
        lastSeq = seq;
        if (found) {
          clearInterval(queueInt); clearInterval(queueTick);
          localStorage.removeItem("overclock_queued");
          toast("Match found", { detail: `Duel #${found} is live — commit your answer.`, kind: "ok" });
          enterDuel(found);
          loadBoard();
        }
      }
    } catch { /* keep polling */ }
  };
  check();
  queueInt = setInterval(check, 3000);
}

const isAlreadyQueuedErr = (err) => /already queued|unreachable/i.test(String(err?.message || "") + JSON.stringify(err));

async function doJoin() {
  if (needWallet()) return;
  const casual = mode === "casual";
  const stake = TIER_STAKE[tier];
  queueView("Confirm in your wallet…", `Staking ${stake} NEAR on tier ${tier} (${mode}).`, false);
  try {
    const res = await call("join_queue", { tier, casual }, utils.format.parseNearAmount(stake));
    localStorage.setItem("overclock_queued", JSON.stringify({ tier, casual }));
    queueView(
      "Looking for an opponent…",
      `${casual ? "Casual" : "Ranked"} · tier ${tier} · ${stake} NEAR · waiting <span id="q-elapsed" class="mono">0s</span>`
    );
    toast("You're in the queue", { detail: "We'll open the duel as soon as someone matches.", tx: txHash(res), kind: "ok" });
    startMatchPolling(Date.now());
  } catch (err) {
    if (isAlreadyQueuedErr(err)) {
      // Most often: refresh/back left a stale queue. Recover gracefully.
      let q = null;
      try { q = JSON.parse(localStorage.getItem("overclock_queued") || "null"); } catch { /* ignore */ }
      const same = q && q.tier === tier && !!q.casual === casual;
      try {
        const p = await viewC.get_player({ account: me() }).catch(() => null);
        if (p?.queued && same) {
          localStorage.setItem("overclock_queued", JSON.stringify({ tier, casual }));
          queueView(
            "Still in the queue",
            `${casual ? "Casual" : "Ranked"} · tier ${tier} · ${stake} NEAR — resumed`
          );
          toast("Resumed your queue", { detail: "You were already queued — restored.", kind: "ok" });
          startMatchPolling(Date.now());
          return;
        }
      } catch { /* fall through */ }
      leaveToLobby();
      toast("Already queued", {
        detail: "You're already in a queue on-chain (refresh/back can leave it active). Refresh the page to restore it, or use Leave queue on the restored view.",
        kind: "bad",
      });
      return;
    }
    leaveToLobby();
    toast("Couldn't join the queue", { detail: errMsg(err), kind: "bad" });
  }
}
$("join").addEventListener("click", doJoin);

async function cancelQueue() {
  let q = null;
  try { q = JSON.parse(localStorage.getItem("overclock_queued") || "null"); } catch { /* ignore */ }
  if (!q) {
    toast("Queue record missing", { detail: "Restoring from chain…", kind: "bad" });
    localStorage.removeItem("overclock_queued");
    clearInterval(queueInt); clearInterval(queueTick);
    reconcileQueue();
    return;
  }
  await busy($("cancel-queue"), async () => {
    try {
      const res = await call("cancel_queue", { tier: q.tier, casual: q.casual });
      localStorage.removeItem("overclock_queued");
      toast("Left the queue", { detail: "Your stake was refunded.", tx: txHash(res), kind: "ok" });
      leaveToLobby();
    } catch (err) {
      const msg = errMsg(err);
      if (/not queued/i.test(msg + String(err))) {
        localStorage.removeItem("overclock_queued");
        clearInterval(queueInt); clearInterval(queueTick);
        leaveToLobby();
        toast("Queue already cleared", { detail: "You're not in that queue on-chain anymore.", kind: "ok" });
      } else {
        toast("Couldn't leave the queue", { detail: msg, kind: "bad" });
      }
    }
  });
}

async function resumeDuel() {
  try {
    const cfg = await viewC.get_config();
    const id = await scanForMyDuel(Number(cfg.duel_seq));
    if (id) enterDuel(id);
  } catch { /* ignore */ }
}

// Restore queue state after refresh/back — fixes "stuck queued, can't rejoin".
// The contract traps join_queue if already queued, so we must reconcile.
async function reconcileQueue() {
  if (!me() || !viewC || currentDuelId) return;
  try {
    const [p, cfg] = await Promise.all([
      viewC.get_player({ account: me() }).catch(() => null),
      viewC.get_config().catch(() => null),
    ]);
    // An in-flight duel takes priority over queue state.
    if (cfg) {
      const id = await scanForMyDuel(Number(cfg.duel_seq));
      if (id) {
        localStorage.removeItem("overclock_queued");
        enterDuel(id);
        return;
      }
    }
    if (!p?.queued) {
      localStorage.removeItem("overclock_queued");
      return;
    }
    let q = null;
    try { q = JSON.parse(localStorage.getItem("overclock_queued") || "null"); } catch { /* ignore */ }
    if (q && Number.isFinite(q.tier)) {
      queueView(
        "Still in the queue",
        `${q.casual ? "Casual" : "Ranked"} · tier ${q.tier} · ${TIER_STAKE[q.tier]} NEAR — restored after refresh`
      );
      toast("Queue restored", { detail: "You're still queued — scanning for a match.", kind: "ok" });
      startMatchPolling(Date.now());
      return;
    }
    // Queued on-chain but we don't know tier/mode (different browser/session).
    // Show recovery so the user can cancel without guessing blindly.
    enterDuelView(`
      <div class="stage">
        <div class="stage-head"><div class="stage-title">${pill("Queue active", "volt")} Stuck in queue</div></div>
        <p class="muted">This account is queued on-chain (maybe from another tab or a refresh), but this browser doesn't know which tier. Pick the tier/mode you joined to leave and get your stake back.</p>
        <div class="actions" style="flex-wrap:wrap;gap:8px;margin-top:12px">
          ${[1,2,3].flatMap(t => [true,false].map(c =>
            `<button class="btn stale-cancel" data-tier="${t}" data-casual="${c}" type="button">Cancel T${t} ${c ? "Casual" : "Ranked"}</button>`
          )).join("")}
        </div>
        <div class="actions" style="margin-top:8px"><button id="stale-back" class="btn btn-ghost" type="button">Back</button></div>
      </div>`);
    document.querySelectorAll(".stale-cancel").forEach((btn) => {
      btn.addEventListener("click", async () => {
        const t = Number(btn.dataset.tier);
        const c = btn.dataset.casual === "true";
        await busy(btn, async () => {
          try {
            const res = await call("cancel_queue", { tier: t, casual: c });
            localStorage.removeItem("overclock_queued");
            toast("Left the queue", { detail: `Canceled tier ${t} ${c ? "casual" : "ranked"}. Stake refunded.`, tx: txHash(res), kind: "ok" });
            leaveToLobby();
          } catch (err) {
            const msg = errMsg(err);
            if (/not queued/i.test(msg + String(err))) {
              toast("Not that one", { detail: `You're not in tier ${t} ${c ? "casual" : "ranked"} — try another.`, kind: "bad" });
            } else {
              toast("Couldn't cancel", { detail: msg, kind: "bad" });
            }
          }
        });
      });
    });
    $("stale-back")?.addEventListener("click", leaveToLobby);
  } catch { /* non-fatal */ }
}

// ───────────────── duel stage ─────────────────
function enterDuel(id) {
  clearInterval(tickInt); clearInterval(pollInt);
  currentDuelId = id; currentProblem = null; currentSalt = null; lastSig = ""; loadingProblem = false;
  refreshDuel(true);
  pollInt = setInterval(() => refreshDuel(false), 4000);
}

async function loadProblem() {
  if (!currentDuelId || loadingProblem) return;
  loadingProblem = true;
  currentProblem = await viewC.get_problem({ duel_id: currentDuelId }).catch(() => null);
  loadingProblem = false;
  refreshDuel(true);
}

function readStored() {
  try { return JSON.parse(localStorage.getItem(`overclock_commit_${currentDuelId}`) || "null"); } catch { return null; }
}

async function refreshDuel(force = true) {
  if (!currentDuelId) return;
  const d = await viewC.get_duel({ duel_id: currentDuelId }).catch(() => null);
  if (!d) {
    if (force) {
      enterDuelView(`<div class="queue"><h3>Duel #${currentDuelId}</h3><p class="muted">Not found on-chain yet…</p><button id="queue-back" class="btn btn-ghost" type="button">Back</button></div>`);
      $("queue-back")?.addEventListener("click", leaveToLobby);
    }
    return;
  }
  const sig = JSON.stringify([d.settled, d.draw, d.winner, d.hash_a, d.hash_b, d.order_a, d.order_b, d.deadline, d.reveal_deadline, !!currentProblem]);
  if (!force && sig === lastSig && $("stage")) return;
  lastSig = sig;
  renderDuel(d);
}

function sideHtml(d, which) {
  const who = d[which];
  const hash = d[`hash_${which}`];
  const order = d[`order_${which}`];
  const mine = who === me();
  return `<div class="side ${mine ? "me" : ""}">
    <div class="side-name" title="${esc(who)}">${esc(who)}${mine ? " (you)" : ""}</div>
    <div class="side-state ${hash ? "done" : ""}">${hash ? `✓ Committed${order ? ` · #${esc(order)}` : ""}` : "Waiting to commit…"}</div>
  </div>`;
}

function renderDuel(d) {
  const who = me();
  const mySide = who === d.a ? 0 : who === d.b ? 1 : -1;
  if (d.settled) return renderSettled(d, mySide);

  clearInterval(queueInt); clearInterval(queueTick);
  const draftEl = document.querySelector("#answer-form input[name=answer]");
  const draft = draftEl && !draftEl.readOnly ? draftEl.value : "";
  const stored = readStored();
  const hasCommit = !!(stored && (stored.a === d.a || stored.a === d.b) && (stored.b === d.a || stored.b === d.b));
  if (hasCommit) currentSalt = stored.salt;
  const both = !!(d.hash_a && d.hash_b);
  const commitAt = Number(d.deadline);
  const revealAt = Number(d.reveal_deadline || d.deadline);

  if (!currentProblem && !loadingProblem) loadProblem();

  enterDuelView(`
    <div class="stage" id="stage" data-commit="${commitAt}" data-reveal="${revealAt}" data-both="${both ? 1 : 0}" data-committed="${hasCommit ? 1 : 0}" data-mine="${mySide >= 0 ? 1 : 0}">
      <div class="stage-head">
        <div class="stage-title">Duel #${esc(d.id)} ${pill("Live", "ok")} ${pill(`Tier ${d.tier}`, "volt")}</div>
        <span class="mini-meta"><span>stake ${fmtYocto(d.stake)} each</span></span>
      </div>

      <div class="versus">${sideHtml(d, "a")}<span class="vs">VS</span>${sideHtml(d, "b")}</div>

      <ol class="phases" id="phases">
        <li data-p="commit"><i>1</i>Commit</li>
        <li data-p="reveal"><i>2</i>Reveal</li>
        <li data-p="settle"><i>3</i>Settle</li>
      </ol>

      <div class="problem-card">
        <div class="problem-label"><span>Problem</span><span class="countdown" id="cd">—</span></div>
        <div class="problem-text">${currentProblem ? esc(currentProblem.text) : "Loading problem…"}</div>
        <div class="timebar"><i id="timebar"></i></div>
      </div>

      ${mySide >= 0 ? `
      <form id="answer-form" class="answer" autocomplete="off">
        <input name="answer" inputmode="numeric" placeholder="Your answer" aria-label="Your answer" required />
        <button type="submit" class="btn btn-primary" id="commit-btn">Lock in answer</button>
      </form>` : `<p class="help">You are watching this duel — only the two players can answer.</p>`}

      <p class="help" id="help"></p>

      ${mySide >= 0 ? `<div class="actions">
        <button id="reveal-btn" class="btn" type="button">Reveal &amp; verify</button>
        <button id="settle-btn" class="btn" type="button">Settle pot</button>
      </div>` : `<div class="actions"><button id="settle-btn" class="btn" type="button">Settle pot</button></div>`}
    </div>`);

  const input = document.querySelector("#answer-form input[name=answer]");
  if (input) {
    if (hasCommit) {
      input.value = stored.answer; input.readOnly = true; input.classList.add("locked");
      $("commit-btn").disabled = true;
    } else if (draft) {
      input.value = draft;
    }
    if (!hasCommit && !document.activeElement?.closest?.("#answer-form")) input.focus({ preventScroll: true });
  }

  $("answer-form")?.addEventListener("submit", onCommit);
  $("reveal-btn")?.addEventListener("click", onReveal);
  $("settle-btn")?.addEventListener("click", () => onSettle($("settle-btn")));
  clearInterval(tickInt);
  tickInt = setInterval(tick, 250);
  tick();
}

function tick() {
  const st = $("stage");
  if (!st) return;
  const now = Date.now();
  const cT = Number(st.dataset.commit);
  const rT = Number(st.dataset.reveal) || cT;
  const both = st.dataset.both === "1";
  const committed = st.dataset.committed === "1";
  const mine = st.dataset.mine === "1";
  let phase, left, total;
  if (now < cT) { phase = "commit"; left = cT - now; total = COMMIT_MS; }
  else if (now < rT) { phase = "reveal"; left = rT - now; total = REVEAL_MS; }
  else { phase = "settle"; left = 0; total = 1; }

  const cd = $("cd");
  cd.textContent = phase === "settle" ? "Windows closed" : `${Math.ceil(left / 1000)}s`;
  cd.className = `countdown ${phase === "settle" ? "done" : left > 10_000 ? "calm" : ""}`;
  $("timebar").style.transform = `scaleX(${Math.max(0, Math.min(1, left / total))})`;

  const order = ["commit", "reveal", "settle"];
  $("phases").querySelectorAll("li").forEach((li) => {
    const idx = order.indexOf(li.dataset.p);
    li.classList.toggle("active", li.dataset.p === phase);
    li.classList.toggle("done", idx < order.indexOf(phase));
  });

  const canReveal = both || now > cT;
  const commitBtn = $("commit-btn");
  if (commitBtn && !committed) commitBtn.disabled = phase !== "commit";
  const rb = $("reveal-btn");
  if (rb && !rb.classList.contains("is-busy")) {
    rb.disabled = !canReveal || phase === "settle";
    rb.classList.toggle("btn-primary", !rb.disabled && committed);
  }
  const sb = $("settle-btn");
  if (sb) sb.classList.toggle("btn-primary", phase === "settle");

  let help = "", cls = "help";
  if (!mine) help = "";
  else if (phase === "commit" && !committed) help = "Type your answer and lock it in before the timer runs out. Only a hash is sent — nobody sees your answer yet.";
  else if (phase === "commit" && committed && !both) help = "Answer locked in. Waiting for your opponent to commit…";
  else if (phase === "commit" && committed) { help = "Both players committed — you can reveal now."; cls = "help ok"; }
  else if (phase === "reveal" && committed) { help = "Reveal your answer so the contract can verify it."; cls = "help ok"; }
  else if (phase === "reveal") help = "The commit window closed and you did not commit.";
  else help = "Both windows are closed — settle to release the pot.";
  const h = $("help");
  if (h) { h.textContent = help; h.className = cls; }
}

async function onCommit(e) {
  e.preventDefault();
  const who = me();
  const input = e.target.querySelector("input[name=answer]");
  const ans = input.value.trim();
  if (!/^-?\d+$/.test(ans)) return toast("Enter a whole number", { kind: "bad" });
  const d = await viewC.get_duel({ duel_id: currentDuelId }).catch(() => null);
  currentSalt = currentSalt || `salt-${crypto.getRandomValues(new Uint32Array(1))[0]}`;
  const hash = await sha256hex(`${currentDuelId}:${who}:${ans}:${currentSalt}`);
  await busy($("commit-btn"), async () => {
    try {
      const res = await call("submit_answer", { duel_id: currentDuelId, answer_hash: hash });
      localStorage.setItem(`overclock_commit_${currentDuelId}`, JSON.stringify({ a: d?.a ?? who, b: d?.b ?? who, answer: ans, salt: currentSalt, hash }));
      toast("Answer locked in", { detail: "Reveal once both sides have committed.", tx: txHash(res), kind: "ok" });
      refreshDuel(true);
    } catch (err) {
      toast("Commit failed", { detail: errMsg(err), kind: "bad" });
    }
  });
}

async function onReveal() {
  const fresh = await viewC.get_duel({ duel_id: currentDuelId }).catch(() => null);
  // Reveal guard: revealing before both sides commit would leak the answer while the opponent is still open.
  const ready = fresh && ((fresh.hash_a && fresh.hash_b) || Date.now() > Number(fresh.deadline));
  if (!ready) return toast("Reveal is locked", { detail: "Wait until both sides commit or the commit window ends.", kind: "bad" });
  const stored = readStored();
  const inputAns = document.querySelector("#answer-form input[name=answer]")?.value?.trim();
  const ans = stored?.answer ?? inputAns;
  const salt = stored?.salt ?? currentSalt;
  if (!ans || !salt) return toast("Nothing to reveal", { detail: "No commitment is stored in this browser for this duel.", kind: "bad" });
  if (!Number.isFinite(Number(ans))) return toast("Bad answer", { detail: "The answer must be an integer.", kind: "bad" });
  await busy($("reveal-btn"), async () => {
    try {
      const res = await call("reveal", { duel_id: currentDuelId, answer: Math.trunc(Number(ans)), salt });
      localStorage.removeItem(`overclock_commit_${currentDuelId}`);
      toast("Answer revealed", { detail: "Settle to release the pot once the windows close.", tx: txHash(res), kind: "ok" });
      refreshDuel(true);
    } catch (err) {
      toast("Reveal failed", { detail: errMsg(err), kind: "bad" });
    }
  });
}

async function onSettle(btn, id = currentDuelId) {
  await busy(btn, async () => {
    try {
      const res = await call("settle", { duel_id: id }, "0", 150_000_000_000_000n);
      toast("Duel settled", { tx: txHash(res), kind: "ok" });
      if (id === currentDuelId) refreshDuel(true);
      loadBoard(); loadStats(); loadProfile();
    } catch (err) {
      toast("Settle failed", { detail: errMsg(err), kind: "bad" });
    }
  });
}

function renderSettled(d, mySide) {
  clearInterval(tickInt); clearInterval(pollInt);
  const who = me();
  const stakeN = nearOf(d.stake);
  let cls = "draw", headline = "Draw", amount = `≈ ${fmtNear(stakeN * (1 - RAKE))} back to each`, note = "No correct reveal on either side — stakes returned minus the 3% rake.";
  if (!d.draw) {
    const won = d.winner === who;
    if (mySide >= 0) {
      cls = won ? "win" : "loss";
      headline = won ? "You won" : "You lost";
      amount = won ? `+ ≈ ${fmtNear(stakeN * 2 * (1 - RAKE))}` : `− ${fmtNear(stakeN)}`;
      note = won ? "Pot minus the 3% rake, paid out by the contract." : "Your opponent's answer was correct (or first). Rematch?";
    } else {
      headline = "Settled"; amount = `Winner: ${d.winner || "—"}`; note = "";
    }
  }
  enterDuelView(`
    <div class="stage">
      <div class="stage-head">
        <div class="stage-title">Duel #${esc(d.id)} ${pill(d.draw ? "Draw" : "Settled", d.draw ? "" : "ok")} ${pill(`Tier ${d.tier}`, "volt")}</div>
      </div>
      <div class="result ${cls}">
        <h3>${esc(headline)}</h3>
        <div class="amount">${esc(amount)}</div>
        ${note ? `<p>${esc(note)}</p>` : ""}
      </div>
      <div class="versus">
        <div class="side ${d.a === who ? "me" : ""}"><div class="side-name">${esc(d.a)}${d.a === who ? " (you)" : ""}</div><div class="side-state ${d.winner === d.a ? "done" : ""}">${d.winner === d.a ? "✓ Winner" : "—"}</div></div>
        <span class="vs">VS</span>
        <div class="side ${d.b === who ? "me" : ""}"><div class="side-name">${esc(d.b)}${d.b === who ? " (you)" : ""}</div><div class="side-state ${d.winner === d.b ? "done" : ""}">${d.winner === d.b ? "✓ Winner" : "—"}</div></div>
      </div>
      <div class="actions">
        <button id="again" class="btn btn-primary" type="button">Play again</button>
        <button id="settle-again" class="btn" type="button">Refresh</button>
      </div>
    </div>`);
  $("again").addEventListener("click", leaveToLobby);
  $("settle-again").addEventListener("click", () => refreshDuel(true));
  loadProfile(); loadBoard(); loadStats();
}

// ───────────────── profile / accreditation ─────────────────
async function loadProfile() {
  const who = me();
  playerInfo = null;
  if (who && viewC) playerInfo = await viewC.get_player({ account: who }).catch(() => null);
  renderProfile();
  renderCta();
  loadClaimable();
}

function renderProfile() {
  const who = me();
  const p = playerInfo;
  const elo = Number(p?.elo) > 0 ? p.elo : null;
  $("profile").innerHTML = `
    <div><small>ELO</small><b>${elo ?? "—"}</b></div>
    <div><small>XP</small><b>${p?.xp ?? "—"}</b></div>
    <div><small>W / L</small><b>${p ? `${p.wins ?? 0}/${p.losses ?? 0}` : "—"}</b></div>
    <div class="wide"><small>Status</small>${
      !who ? pill("Not connected") : elo ? pill("Accredited", "ok") : p ? pill("Registered — not accredited", "volt") : pill("Not registered")}</div>`;

  const done = { connect: !!who, register: !!p, placement: !!elo };
  let nextFound = false;
  $("checklist").querySelectorAll("li").forEach((li) => {
    const step = li.dataset.step;
    const isDone = done[step];
    li.classList.toggle("done", isDone);
    const isNext = !isDone && !nextFound;
    if (!isDone) nextFound = true;
    li.classList.toggle("next", isNext);
    li.classList.toggle("locked", !isDone && !isNext);
  });
  $("register").disabled = !who;
  $("run-placement").disabled = !who;
}

async function loadClaimable() {
  const who = me();
  const el = $("claimable");
  if (!who || !viewC) { el.textContent = "—"; return; }
  try {
    const v = await viewC.get_claimable({ account: who });
    const raw = typeof v === "object" && v !== null ? (v.amount ?? v.claimable ?? 0) : v;
    el.textContent = fmtYocto(raw);
  } catch { el.textContent = "—"; }
}

$("register").addEventListener("click", () => {
  if (needWallet()) return;
  busy($("register"), async () => {
    try {
      const res = await call("register_agent", {}, "5120000000000000000000");
      toast("Agent registered", { detail: "0.00512 NEAR deposit, refundable via “Refund storage”.", tx: txHash(res), kind: "ok" });
      loadProfile();
    } catch (err) { toast("Registration failed", { detail: errMsg(err), kind: "bad" }); }
  });
});

$("claim-reward").addEventListener("click", () => {
  if (needWallet()) return;
  busy($("claim-reward"), async () => {
    try {
      const res = await call("claim_reward", {}, "0", 100_000_000_000_000n);
      toast("Rewards claimed", { tx: txHash(res), kind: "ok" });
      loadStats(); loadClaimable();
    } catch (err) { toast("Nothing to claim", { detail: errMsg(err), kind: "bad" }); }
  });
});

$("refund-storage").addEventListener("click", () => {
  if (needWallet()) return;
  busy($("refund-storage"), async () => {
    try {
      const res = await call("refund_storage_deposit", {}, "0", 50_000_000_000_000n);
      toast("Storage deposit refunded", { tx: txHash(res), kind: "ok" });
      loadStats();
    } catch (err) { toast("Refund failed", { detail: errMsg(err), kind: "bad" }); }
  });
});

// Placement: start on-chain → fetch the 20 problems → solve locally → submit answers.
$("run-placement").addEventListener("click", async () => {
  if (needWallet()) return;
  const dlg = $("placement-dialog");
  const out = $("placement-out");
  $("placement-sub").textContent = "Confirm the 0.00512 NEAR refundable deposit in your wallet…";
  out.innerHTML = `<div class="bar"><i style="width:20%"></i></div>`;
  if (!dlg.open) dlg.showModal();
  try {
    await call("start_placement", {}, "5120000000000000000000");
    const problems = await viewC.get_placement_problems({ account: me() });
    const answers = problems.map((p) => {
      const a = parseSolve(p.text);
      return Number.isFinite(a) ? a : 0;
    });
    $("placement-sub").textContent = `${answers.length} problems drawn and solved locally. Review, then submit — the contract scores you.`;
    out.innerHTML = `
      <div class="pgrid">${problems.map((p, i) => `<div class="pitem"><span>${i + 1}</span><span>${esc(p.text.replace(" = ?", ""))}</span><b>${esc(answers[i])}</b></div>`).join("")}</div>
      <button id="placement-submit" class="btn btn-primary btn-xl" type="button">Submit ${answers.length} answers</button>`;
    $("placement-submit").addEventListener("click", () => busy($("placement-submit"), async () => {
      try {
        const res = await call("submit_placement", { answers: answers.map((a) => Math.trunc(a)) });
        toast("Placement submitted", { detail: "Your ELO was seeded on-chain by the contract.", tx: txHash(res), kind: "ok" });
        dlg.close();
        loadBoard(); loadProfile();
      } catch (err) {
        toast("Submit failed", { detail: errMsg(err), kind: "bad" });
      }
    }));
  } catch (err) {
    $("placement-sub").textContent = "Something went wrong.";
    out.innerHTML = `<div class="note warn">${esc(errMsg(err))}</div>`;
  }
});

// ───────────────── watch ─────────────────
$("watch-duel").addEventListener("click", () => busy($("watch-duel"), async () => {
  const out = $("watch-out");
  try {
    const cfg = await viewC.get_config();
    watchId = Number(cfg.duel_seq);
    if (!watchId) { out.innerHTML = `<p class="muted">No duels yet.</p>`; return; }
    const d = await viewC.get_duel({ duel_id: watchId });
    const p = await viewC.get_problem({ duel_id: watchId }).catch(() => null);
    out.innerHTML = `
      <div class="stage-title">Duel #${esc(d.id)} ${d.settled ? pill(d.draw ? "Draw" : "Settled", d.draw ? "" : "ok") : pill("Live", "ok")}</div>
      <div class="mono">${esc(d.a)} <span class="muted">vs</span> ${esc(d.b)}</div>
      <div class="mono">${esc(p?.text ?? "—")}</div>
      <div class="mini-meta"><span>winner: ${esc(d.winner || "—")}</span><span>stake each: ${fmtYocto(d.stake)}</span></div>`;
  } catch (err) {
    out.innerHTML = `<div class="note warn">${esc(errMsg(err))}</div>`;
  }
}));
$("watch-settle").addEventListener("click", () => {
  if (needWallet()) return;
  if (!watchId) return toast("Load a duel first", { kind: "bad" });
  onSettle($("watch-settle"), watchId);
});

// ───────────────── leaderboard / stats ─────────────────
function renderBoard() {
  const who = me();
  const head = `<div class="board-row head"><span>#</span><span>Player</span><span class="num">ELO</span><span class="num">XP</span><span class="num">W / L</span></div>`;
  const body = boardRows.length
    ? boardRows.map((r) => `
        <div class="board-row ${r.account === who ? "me" : ""}">
          <span class="rank r${r.rank}">${esc(r.rank)}</span>
          <a class="who-cell" href="${acctLink(r.account)}" target="_blank" rel="noreferrer" title="${esc(r.account)}">${esc(r.account)}</a>
          <span class="num">${esc(r.elo)}</span><span class="num">${esc(r.xp)}</span><span class="num">${esc(r.wins)}/${esc(r.losses)}</span>
        </div>`).join("")
    : `<div class="board-empty">No accredited players yet — be the first.</div>`;
  $("board").innerHTML = (boardRows.length ? head : "") + body;
}

async function loadBoard() {
  try {
    boardRows = await viewC.get_leaderboard({ limit: 10 });
    renderBoard();
  } catch (e) {
    $("board").innerHTML = `<div class="board-empty">Leaderboard unavailable — ${esc(errMsg(e))}</div>`;
  }
}
$("refresh-board").addEventListener("click", () => busy($("refresh-board"), loadBoard));

async function loadStats() {
  try {
    const cfg = await viewC.get_config();
    const top = await viewC.get_season_top().catch(() => []);
    const stat = (k, v, soft = false) => `<div class="stat"><span class="stat-label">${k}</span><span class="stat-value ${soft ? "soft" : ""}">${v}</span></div>`;
    $("stats").innerHTML = [
      stat("Season", `#${Number(cfg.season) + 1}`),
      stat("Treasury", fmtYocto(cfg.treasury)),
      stat("Burned", fmtYocto(cfg.burn)),
      stat("Duels played", Number(cfg.duel_seq)),
      stat("Top 5 now", top.length ? top.map((t) => esc(t.account.split(".")[0])).join(", ") : "—", true),
    ].join("");
  } catch {
    $("stats").innerHTML = `<div class="stat" style="grid-column:1/-1"><span class="stat-label">Status</span><span class="stat-value soft">Contract not reachable yet — showing the arena in read-only mode.</span></div>`;
  }
}

// ───────────────── boot ─────────────────
renderCta();
renderProfile();
renderBoard();
await initView().catch((e) => console.error(e));
await initWallet().catch((e) => { console.error(e); $("who").textContent = "Wallet unavailable"; });
loadStats();
loadBoard();
