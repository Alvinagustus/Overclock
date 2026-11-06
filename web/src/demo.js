// Demo mode (?demo) — a tiny in-memory stand-in for the contract so the UI can
// be explored without a wallet or a deployed contract. Never used in production.
const NPC = "npc-pro.testnet";
const Y = (near) => (BigInt(Math.round(near * 1e6)) * 10n ** 18n).toString();

export function createDemo(me) {
  let seq = 0;
  let duel = null;
  let accredited = false;
  let registered = false;

  const board = [
    ["nova-agent.testnet", 1640, 2210, 31, 9],
    ["prime-solver.testnet", 1585, 1980, 27, 11],
    ["modbot.testnet", 1522, 1640, 22, 12],
    ["quark.testnet", 1470, 1310, 19, 14],
    ["euler-9.testnet", 1388, 990, 15, 15],
    ["gcd-gremlin.testnet", 1290, 720, 11, 13],
    [me, 1240, 860, 14, 6],
    ["bitflip.testnet", 1120, 410, 7, 12],
  ]
    .sort((a, b) => b[1] - a[1])
    .map(([account, elo, xp, wins, losses], i) => ({ rank: i + 1, account, elo, xp, wins, losses }));

  const placement = [
    "12 + 34 - 5 = ?", "17 * 23 = ?", "245 mod 17 = ?", "(37 * 91) mod 53 = ?", "gcd(84, 126) = ?",
    "7^13 mod 31 = ?", "x = ? such that 7 * x = 1 (mod 31), 0 < x < 31", "popcount(181 XOR 94) = ?",
    "88 + 45 - 19 = ?", "31 * 29 = ?", "1000 mod 37 = ?", "(45 * 67) mod 101 = ?", "gcd(144, 60) = ?",
    "3^20 mod 17 = ?", "x = ? such that 5 * x = 1 (mod 23), 0 < x < 23", "popcount(255 XOR 15) = ?",
    "9 + 91 - 40 = ?", "14 * 14 = ?", "999 mod 7 = ?", "(12 * 12) mod 11 = ?",
  ].map((text, i) => ({ id: i, text }));

  const viewC = {
    async get_config() {
      return { season: 2, treasury: Y(12.4), burn: Y(8.2), duel_seq: seq };
    },
    async get_season_top() { return board.slice(0, 5).map((r) => ({ account: r.account })); },
    async get_leaderboard({ limit = 10 }) { return board.slice(0, limit); },
    async get_player() { return registered || accredited ? { elo: accredited ? 1240 : 0, xp: 860, wins: 14, losses: 6 } : null; },
    async get_claimable() { return Y(3.4); },
    async get_duel({ duel_id }) { return duel && duel.id === duel_id ? { ...duel } : null; },
    async get_problem() { return { text: "(37 * 91) mod 53 = ?" }; },
    async get_placement_problems() { return placement; },
  };

  const tx = (m) => ({ transaction: { hash: `DEMO${m.toUpperCase().slice(0, 4)}${Date.now().toString(36)}` } });
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));

  async function call(method, args) {
    await wait(500);
    switch (method) {
      case "register_agent": registered = true; break;
      case "start_placement": registered = true; break;
      case "submit_placement": accredited = true; break;
      case "join_queue":
        setTimeout(() => {
          seq += 1;
          const now = Date.now();
          duel = {
            id: seq, a: me, b: NPC, tier: args.tier, stake: Y(args.tier === 1 ? 0.01 : args.tier === 2 ? 0.1 : 1),
            deadline: now + 30000, reveal_deadline: now + 50000, settled: false, draw: false, winner: null,
            hash_a: null, hash_b: null, order_a: 0, order_b: 0, correct: null,
          };
        }, 2600);
        break;
      case "cancel_queue": break;
      case "submit_answer":
        duel.hash_a = args.answer_hash; duel.order_a = 1;
        setTimeout(() => { if (duel) { duel.hash_b = "npc-hash"; duel.order_b = 2; } }, 2200);
        break;
      case "reveal":
        duel.correct = Number(args.answer) === 28;
        break;
      case "settle":
        duel.settled = true;
        duel.winner = duel.correct ? me : NPC;
        break;
      default: break;
    }
    return tx(method);
  }
  return { viewC, call };
}
