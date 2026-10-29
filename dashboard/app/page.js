"use client";

import { useEffect, useState, useCallback, useRef } from "react";

const REFRESH_MS = 10000; // full state
const PRICE_MS = 3000; // lightweight oracle price
const HISTORY_MAX = 60; // sparkline samples (~3 min at 3s)
const EXPLORER = (addr) =>
  `https://explorer.solana.com/address/${addr}?cluster=devnet`;
const EXPLORER_TX = (sig) =>
  `https://explorer.solana.com/tx/${sig}?cluster=devnet`;

const short = (a) => (a ? `${a.slice(0, 4)}…${a.slice(-4)}` : "—");
const daysAgo = (iso) =>
  Math.floor((Date.now() - new Date(iso).getTime()) / 86400000);
const ago = (unix) => {
  if (!unix) return "—";
  const s = Math.floor(Date.now() / 1000 - unix);
  if (s < 60) return `${s}s ago`;
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
};

function Addr({ value }) {
  return (
    <a className="addr mono" href={EXPLORER(value)} target="_blank" rel="noreferrer">
      {value} ↗
    </a>
  );
}

// Count-up: eases from the previous value to the new one.
function AnimatedNumber({ value, decimals = 0, className }) {
  const [disp, setDisp] = useState(value);
  const fromRef = useRef(value);
  const rafRef = useRef();
  useEffect(() => {
    const from = fromRef.current;
    const to = value;
    if (from === to) return;
    const dur = 500;
    const t0 = performance.now();
    cancelAnimationFrame(rafRef.current);
    const tick = (t) => {
      const p = Math.min(1, (t - t0) / dur);
      const eased = 1 - Math.pow(1 - p, 3);
      setDisp(from + (to - from) * eased);
      if (p < 1) rafRef.current = requestAnimationFrame(tick);
      else fromRef.current = to;
    };
    rafRef.current = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafRef.current);
  }, [value]);
  return (
    <span className={className}>
      {disp.toLocaleString(undefined, { minimumFractionDigits: decimals, maximumFractionDigits: decimals })}
    </span>
  );
}

function ThemeToggle() {
  const [theme, setTheme] = useState("dark");
  useEffect(() => {
    setTheme(document.documentElement.dataset.theme || "dark");
  }, []);
  const toggle = () => {
    const next = theme === "light" ? "dark" : "light";
    document.documentElement.dataset.theme = next;
    try { localStorage.setItem("theme", next); } catch {}
    setTheme(next);
  };
  return (
    <button className="themebtn" onClick={toggle} title="Toggle light / dark" aria-label="Toggle theme">
      {theme === "light" ? "☾" : "☀"}
    </button>
  );
}


export default function Page() {
  const [s, setS] = useState(null);
  const [err, setErr] = useState(null);
  const [pulse, setPulse] = useState(0);
  const [oracle, setOracle] = useState(null);
  const [history, setHistory] = useState([]);
  const [now, setNow] = useState(() => Date.now() / 1000);

  const load = useCallback(async () => {
    try {
      const r = await fetch("/api/state", { cache: "no-store" });
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      const data = await r.json();
      if (data.error) throw new Error(data.error);
      setS(data);
      setErr(null);
      setPulse((p) => p + 1);
    } catch (e) {
      setErr(e.message);
    }
  }, []);

  // Fast, lightweight price tick — keeps the banner alive between full refreshes.
  const loadPrice = useCallback(async () => {
    try {
      const r = await fetch("/api/price", { cache: "no-store" });
      if (!r.ok) return;
      const o = await r.json();
      if (o?.price) {
        setOracle(o);
        setHistory((h) => [...h.slice(-(HISTORY_MAX - 1)), o.price]);
      }
    } catch {}
  }, []);

  useEffect(() => {
    load();
    const id = setInterval(load, REFRESH_MS);
    return () => clearInterval(id);
  }, [load]);

  useEffect(() => {
    loadPrice();
    const id = setInterval(loadPrice, PRICE_MS);
    return () => clearInterval(id);
  }, [loadPrice]);

  // 1s clock so the "age" counts up smoothly between price ticks.
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(id);
  }, []);

  // Restore the sparkline history from a previous session.
  useEffect(() => {
    try {
      const raw = localStorage.getItem("priceHistory");
      if (raw) {
        const arr = JSON.parse(raw);
        if (Array.isArray(arr)) setHistory(arr.slice(-HISTORY_MAX));
      }
    } catch {}
  }, []);

  // Persist history so the chart survives a reload.
  useEffect(() => {
    if (history.length) {
      try { localStorage.setItem("priceHistory", JSON.stringify(history.slice(-HISTORY_MAX))); } catch {}
    }
  }, [history]);

  const displayOracle = oracle ?? s?.oracle ?? null;

  return (
    <div className="wrap">
      <header>
        <h1>
          Sakura <span>Perps</span>
        </h1>
        <div className="hright">
          <div className="live">
            <span className={`dot ${err ? "bad" : ""}`} key={pulse} />
            {err ? `error: ${err}` : s ? `live · ${new Date(s.fetchedAt).toLocaleTimeString()}` : "connecting…"}
          </div>
          <ThemeToggle />
        </div>
      </header>
      <div className="sub">
        devnet monitor · program{" "}
        {s ? (
          <a className="mono" href={EXPLORER(s.programId)} target="_blank" rel="noreferrer">
            {short(s.programId)} ↗
          </a>
        ) : (
          "…"
        )}
        {s && ` · ${s.instructions.length} instructions`}
      </div>

      {!s && !err && <Skeleton />}

      {displayOracle && <LivePrice oracle={displayOracle} history={history} now={now} />}
      {s && <HeroStats s={s} oracle={displayOracle} />}

      <div className="deck">
        {displayOracle?.modes && <GuardCard oracle={displayOracle} />}
        {s?.exchange && <ExchangeCard ex={s.exchange} />}
        {s?.pools.map((p) => <PoolCard key={p.pubkey} p={p} />)}
        {s && <RequestsCard requests={s.requests} />}
        {s && <ActivityCard activity={s.activity} />}
      </div>

      {s && (
        <div className="foot">
          Read-only · sends nothing on-chain · built against the committed IDL,
          no toolchain build required · state {REFRESH_MS / 1000}s · price {PRICE_MS / 1000}s
        </div>
      )}
    </div>
  );
}

function Sparkline({ data, dir }) {
  if (!data || data.length < 2) return <div className="spark ph" />;
  const w = 300, h = 60, pad = 4;
  const min = Math.min(...data), max = Math.max(...data);
  const range = max - min || 1;
  const pts = data.map((v, i) => {
    const x = pad + (i / (data.length - 1)) * (w - 2 * pad);
    const y = pad + (1 - (v - min) / range) * (h - 2 * pad);
    return [x, y];
  });
  const line = pts.map((p, i) => `${i ? "L" : "M"}${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(" ");
  const area = `${line} L${pts[pts.length - 1][0].toFixed(1)},${h - pad} L${pts[0][0].toFixed(1)},${h - pad} Z`;
  const stroke = dir === "down" ? "var(--red)" : "var(--green)";
  const last = pts[pts.length - 1];
  return (
    <svg className="spark" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none">
      <defs>
        <linearGradient id="sparkfill" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={stroke} stopOpacity="0.22" />
          <stop offset="100%" stopColor={stroke} stopOpacity="0" />
        </linearGradient>
      </defs>
      <path d={area} fill="url(#sparkfill)" />
      <path d={line} fill="none" stroke={stroke} strokeWidth="1.6" strokeLinejoin="round" />
      <circle cx={last[0]} cy={last[1]} r="2.6" fill={stroke} className="sparkdot" />
    </svg>
  );
}

function LivePrice({ oracle, history, now }) {
  const price = oracle?.price;
  const prev = history.length >= 2 ? history[history.length - 2] : null;
  const delta = prev != null && price != null ? price - prev : 0;
  const dir = delta > 0.0001 ? "up" : delta < -0.0001 ? "down" : "flat";
  const age = oracle?.publishTime != null ? Math.max(0, Math.floor(now - oracle.publishTime)) : null;
  const verdict = oracle?.modes?.trading?.wouldAccept;
  const hasHist = history.length >= 2;
  const high = hasHist ? Math.max(...history) : null;
  const low = hasHist ? Math.min(...history) : null;
  return (
    <div className="banner">
      <div className="bl">
        <div className="tk">SOL / USD · Pyth oracle</div>
        <div className="pricerow">
          <span key={price} className={`bigprice flash-${dir}`}>
            ${price != null ? price.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 }) : "—"}
          </span>
          {prev != null && (
            <span className={`delta ${dir}`}>
              {dir === "up" ? "▲" : dir === "down" ? "▼" : "•"} {Math.abs(delta).toFixed(3)}
              <span className="dim"> ({((delta / prev) * 100).toFixed(2)}%)</span>
            </span>
          )}
        </div>
        <div className="bmeta">
          <span className={`pill ${verdict ? "ok" : "warn"}`}>{verdict ? "tradeable" : "guarded"}</span>
          <span className="dim">± {oracle?.confidenceBps != null ? oracle.confidenceBps.toFixed(1) : "—"} bps</span>
          <span className="agepulse dim" key={oracle?.publishTime}>{age != null ? `${age}s old` : ""}</span>
        </div>
      </div>
      <div className="bright">
        <Sparkline data={history} dir={dir} />
        {hasHist && (
          <div className="hilo">
            <span>H <b>${high.toFixed(2)}</b></span>
            <span>L <b>${low.toFixed(2)}</b></span>
          </div>
        )}
      </div>
    </div>
  );
}

function HeroStats({ s, oracle }) {
  const o = oracle;
  const p = s.pools?.[0];
  const verdict = o?.modes?.trading?.wouldAccept;
  const passing = o ? o.modes.trading.guards.filter((g) => g.pass === true).length : 0;
  return (
    <div className="hero hero3">
      <div className="tile">
        <div className="tk">Guards · trading</div>
        <div className="tv">
          <span className={`pill ${verdict ? "ok" : "warn"}`}>
            {verdict ? "would accept" : "would reject"}
          </span>
        </div>
        <div className="tsub">{o ? `${passing} / 6 checks pass` : ""}</div>
      </div>
      <div className="tile">
        <div className="tk">Pool TVL</div>
        <div className="tv">
          {p ? <AnimatedNumber value={p.deposited} decimals={0} /> : "—"} <span className="unit">USDC</span>
        </div>
        <div className="tsub">{p ? `of ${p.maxAum.toLocaleString()} cap` : ""}</div>
      </div>
      <div className="tile">
        <div className="tk">Utilization</div>
        <div className="tv">
          {p ? <AnimatedNumber value={p.utilizationPct} decimals={0} /> : "—"}%
        </div>
        <div className="tsub">{p ? `max ${p.maxUtilizationPct}%` : ""}</div>
      </div>
    </div>
  );
}

function GuardCard({ oracle }) {
  const [mode, setMode] = useState("trading");
  const active = oracle.modes[mode];
  const other = oracle.modes[mode === "trading" ? "liquidation" : "trading"];
  const verdict = active.wouldAccept;

  return (
    <div className="card span2">
      <h2>
        Oracle guard status
        <a className="feedlink mono" href={EXPLORER(oracle.feed)} target="_blank" rel="noreferrer">
          feed {oracle.feed.slice(0, 4)}…{oracle.feed.slice(-4)} ↗
        </a>
        <span className={`verdict pill ${verdict ? "ok" : "warn"}`}>
          {verdict ? "would accept ✓" : "would reject ✕"}
        </span>
      </h2>

      <div className="toggle">
        {[
          ["trading", "for_trading", "open a position"],
          ["liquidation", "for_liquidation", "liquidate"],
        ].map(([key, name, desc]) => (
          <button
            key={key}
            className={`tg ${mode === key ? "on" : ""}`}
            onClick={() => setMode(key)}
          >
            <span className="mono">{name}</span>
            <span className="tgdesc">{desc}</span>
            <span className={`pill ${oracle.modes[key].wouldAccept ? "ok" : "warn"} tgv`}>
              {oracle.modes[key].wouldAccept ? "accept" : "reject"}
            </span>
          </button>
        ))}
      </div>

      <div className="guards">
        {active.guards.map((g, i) => {
          const differs = g.limit !== other.guards[i].limit;
          return (
            <div className="guard" key={g.label}>
              <span className={`gmark ${g.pass === null ? "na" : g.pass ? "ok" : "bad"}`}>
                {g.pass === null ? "–" : g.pass ? "✓" : "✕"}
              </span>
              <span className="glabel">
                {g.label}
                {differs && <span className="diff" title="differs between trading and liquidation">Δ</span>}
              </span>
              <span className="gval mono">{g.actual}</span>
              <span className={`glim mono ${differs ? "hl" : "dim"}`}>{g.limit}</span>
              <span className={`gerr mono ${g.pass === false ? "bad" : "dim"}`}>{g.error}</span>
            </div>
          );
        })}
      </div>

      <div className="note" style={{ marginTop: 12 }}>
        Same live price, {mode === "trading" ? "tight" : "looser"} thresholds.{" "}
        <span className="hl">Δ</span> marks the three limits that differ — freshness,
        slot age, confidence. Refusing to liquidate isn&apos;t a safe default, so
        liquidation tolerates a more degraded price. Fails closed — any single ✕ stops it.
      </div>
    </div>
  );
}

function ExchangeCard({ ex }) {
  return (
    <div className="card">
      <h2>Exchange</h2>
      <div className="grid">
        <Stat k="Status" v={<span className={`pill ${ex.paused ? "warn" : "ok"}`}>{ex.paused ? "PAUSED" : "LIVE"}</span>} />
        <Stat k="Markets" v={ex.numMarkets} />
        <Stat k="Protocol fee" v={`${ex.protocolFeeBps / 100}%`} />
        <Stat k="Collateral" v="USDC" />
      </div>
      <div className="rows">
        <Row k="Admin" v={<Addr value={ex.admin} />} />
        <Row k="Collateral mint" v={<Addr value={ex.collateralMint} />} />
        <Row
          k="Freeze authority"
          v={ex.isFreezable
            ? <span className="pill warn">freezable · {short(ex.freezeAuthority)}</span>
            : <span className="pill ok">none</span>}
        />
      </div>
    </div>
  );
}

function PoolCard({ p }) {
  const util = Math.min(100, p.utilizationPct);
  const cap = p.maxUtilizationPct;
  return (
    <div className="card">
      <h2>Pool · {short(p.pubkey)}</h2>
      <div className="grid">
        <Stat k="Deposited" v={`${p.deposited} USDC`} />
        <Stat k="Vault balance" v={`${p.vaultBalance ?? "—"} USDC`} />
        <Stat k="Total shares" v={Number(p.totalShares).toLocaleString()} />
        <Stat k="AUM cap" v={`${p.maxAum.toLocaleString()} USDC`} />
      </div>
      <div className="gauge-wrap">
        <div className="gauge-label">
          <span>Utilization {util.toFixed(1)}%</span>
          <span className="dim">max {cap}%</span>
        </div>
        <div className="gauge">
          <div className="gauge-cap" style={{ left: `${cap}%` }} />
          <div
            className={`gauge-fill ${util >= cap ? "hot" : ""}`}
            style={{ width: `${util}%` }}
          />
        </div>
      </div>
      <div className="rows">
        <Row k="Share mint" v={<Addr value={p.shareMint} />} />
        <Row k="Quote vault" v={<Addr value={p.quoteVault} />} />
        <Row k="Fees (dep / wd)" v={`${p.depositFeeBps} / ${p.withdrawFeeBps} bps`} />
        <Row k="Withdraw delay" v={`${p.withdrawDelaySeconds}s`} />
      </div>
    </div>
  );
}

function RequestsCard({ requests }) {
  return (
    <div className="card">
      <h2>Open withdraw requests ({requests.length})</h2>
      {requests.length === 0 && <div className="note">None.</div>}
      {requests.map((r) => (
        <div key={r.pubkey} className="reqblock">
          <Row k="Owner" v={<Addr value={r.owner} />} />
          <Row k="Shares in request" v={Number(r.shares).toLocaleString()} />
          <Row
            k="Requested"
            v={<>{r.requestedAt.slice(0, 10)} <span className="pill pink">{daysAgo(r.requestedAt)}d ago</span></>}
          />
          <Row
            k="Escrow"
            v={r.escrowExists
              ? <span className="pill warn">holds {Number(r.escrowShares).toLocaleString()} shares</span>
              : <span className="pill ok">closed</span>}
          />
        </div>
      ))}
    </div>
  );
}

function ActivityCard({ activity }) {
  return (
    <div className="card">
      <h2>Recent program activity</h2>
      {(!activity || activity.length === 0) && <div className="note">No recent transactions.</div>}
      {activity?.map((a) => (
        <div className="row" key={a.signature}>
          <span className="k">
            <span className={`pill ${a.err ? "warn" : "ok"}`}>{a.err ? "err" : "ok"}</span>{" "}
            {ago(a.blockTime)}
          </span>
          <a className="addr mono" href={EXPLORER_TX(a.signature)} target="_blank" rel="noreferrer">
            {a.signature.slice(0, 14)}… ↗
          </a>
        </div>
      ))}
    </div>
  );
}

const Stat = ({ k, v }) => (
  <div className="stat">
    <div className="k">{k}</div>
    <div className="v">{v}</div>
  </div>
);
const Row = ({ k, v }) => (
  <div className="row">
    <span className="k">{k}</span>
    <span className="rv">{v}</span>
  </div>
);
const Skeleton = () => (
  <>
    {[0, 1, 2].map((i) => (
      <div className="card skeleton" key={i}>
        <div className="sk-line w40" />
        <div className="sk-line w80" />
        <div className="sk-line w60" />
      </div>
    ))}
  </>
);
