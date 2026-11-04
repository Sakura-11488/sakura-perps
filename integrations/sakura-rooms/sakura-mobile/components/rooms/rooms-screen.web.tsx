import React, { useEffect, useRef, useState } from 'react';
import { Link } from 'expo-router';
import Head from 'expo-router/head';
import { useTheme } from '@/lib/theme';
import {
  approveAllocation, exportRoom, formatCredits, listRooms, publishPlan,
  quoteAllocation, runScenario, verifyExport,
  type AllocationInput, type AllocationQuote, type PlanDraft, type RoomView,
} from '@/lib/rooms-client.web';
import '@/components/perps/perps-roadmap.web.css';
import './rooms.web.css';

type Screen = 'plan' | 'allocation' | 'review';
const screens: { id: Screen; label: string }[] = [
  { id: 'plan', label: 'Plan' },
  { id: 'allocation', label: 'My allocation' },
  { id: 'review', label: 'Review' },
];
const emptyPlan = (): PlanDraft => ({ title: '', thesis: '', invalidation: '' });
const phaseLabels = { ready: 'Plan published', open: 'Allocation approved', settled: 'Scenario complete' };
const credits = (value: string) => `${formatCredits(value)} credits`;

function Arrow() {
  return <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><path d="M4 12h16m-6-6 6 6-6 6" /></svg>;
}

function ScenarioChart() {
  return <svg className="rooms-scenario-chart" viewBox="0 0 280 156" role="img" aria-label="Fixed paper scenario: SOL opens at 100, falls to 92, then settles at 104. Not live market data.">
    <path d="M28 35h224M28 72h224M28 110h224" stroke="currentColor" strokeOpacity=".12" />
    <path d="M28 58 140 110 252 32" fill="none" stroke="currentColor" strokeWidth="2" strokeLinejoin="round" />
    <path d="M28 58 140 110 252 32V123H28Z" fill="currentColor" fillOpacity=".04" />
    <circle cx="28" cy="58" r="4" fill="currentColor" /><circle cx="140" cy="110" r="4" fill="currentColor" /><circle cx="252" cy="32" r="4" fill="currentColor" />
    <text x="28" y="43" textAnchor="middle">100</text><text x="140" y="95" textAnchor="middle">92</text><text x="252" y="17" textAnchor="middle">104</text>
    <text x="28" y="145" textAnchor="middle">Open</text><text x="140" y="145" textAnchor="middle">Dip</text><text x="252" y="145" textAnchor="middle">Settle</text>
  </svg>;
}

function ScenarioAside() {
  return <aside className="rooms-aside" aria-label="Paper scenario and boundaries">
    <div className="rooms-aside-intro"><p className="perps-eyebrow">THE PRACTICE SESSION</p><h2>One idea.<br />Three price points.</h2></div>
    <ScenarioChart />
    <p className="rooms-chart-caption">A fixed SOL scenario. Prices are known in advance, so this exercise tests your plan and limits, not trading skill.</p>
    <dl className="rooms-facts">
      <div><dt>Paper contribution limit</dt><dd>100 credits</dd></div>
      <div><dt>Market</dt><dd>SOL · simulated</dd></div>
      <div><dt>Contract</dt><dd>Finite · capped payout</dd></div>
      <div><dt>Allocations per plan</dt><dd>One</dd></div>
    </dl>
    <p className="rooms-note rooms-aside-footer">Just you, in this browser. No live members, shared funds, real deposits, or orders. The paper engine runs in this browser.</p>
  </aside>;
}

function QuoteSummary({ quote, executed = false }: { quote: AllocationQuote; executed?: boolean }) {
  return <section className="rooms-quote" aria-label={executed ? 'Approved paper allocation' : 'Paper allocation quote'}>
    <h3>{executed ? 'Your approved allocation' : 'Before you approve'}</h3>
    <dl className="rooms-facts">
      <div><dt>Position collateral</dt><dd>{credits(quote.collateral)}</dd></div>
      <div><dt>Entry fee</dt><dd>{credits(quote.entryFee)}</dd></div>
      <div className="rooms-quote-total"><dt>Total paper budget</dt><dd>{credits(quote.budget)}</dd></div>
      <div><dt>Position notional</dt><dd>{credits(quote.notional)}</dd></div>
      <div><dt>Profit cap</dt><dd>{credits(quote.profitCap)}</dd></div>
      <div><dt>Maximum position payout</dt><dd>{credits(quote.maxPayout)}</dd></div>
      <div><dt>Unspent budget remainder</dt><dd>{credits(quote.freeBalance)}</dd></div>
    </dl>
    <p className="rooms-note">The total includes the entry fee. The payout cap includes returned collateral; it is not guaranteed profit. All allocated credits may be lost.</p>
  </section>;
}

function PublishedPlan({ plan }: { plan: PlanDraft }) {
  return <dl className="rooms-published-plan">
    <div><dt>Thesis</dt><dd>{plan.thesis}</dd></div>
    <div><dt>What would change my mind</dt><dd>{plan.invalidation}</dd></div>
  </dl>;
}

function dateLabel(value: string) {
  const date = new Date(value);
  return Number.isFinite(date.getTime())
    ? date.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' })
    : 'Local session';
}

export default function RoomsScreen() {
  const { isDark, toggleTheme } = useTheme();
  const [screen, setScreen] = useState<Screen>('plan');
  const [draft, setDraft] = useState<PlanDraft>(emptyPlan);
  const [room, setRoom] = useState<RoomView | null>(null);
  const [history, setHistory] = useState<RoomView[]>([]);
  const [initialLoading, setInitialLoading] = useState(true);
  const [historyLoaded, setHistoryLoaded] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [imported, setImported] = useState(false);
  const [budget, setBudget] = useState('25');
  const [leverage, setLeverage] = useState<1 | 2 | 3>(2);
  const [side, setSide] = useState<'long' | 'short'>('long');
  const [quote, setQuote] = useState<AllocationQuote | null>(null);
  const [quoteError, setQuoteError] = useState('');
  const [quoteLoading, setQuoteLoading] = useState(false);
  const [approved, setApproved] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  const alert = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLElement>(null);
  const pending = useRef(false);
  const budgetNumber = Number(budget);
  const validBudget = /^\d+$/.test(budget) && Number.isInteger(budgetNumber) && budgetNumber >= 1 && budgetNumber <= 100;
  const allocation: AllocationInput = { budget: budgetNumber, leverage, side };
  const locked = Boolean(busy) || initialLoading;
  const showError = (cause: unknown) => setError(cause instanceof Error ? cause.message.slice(0, 300) : 'The paper engine could not complete this action. Reload local history before trying again.');

  useEffect(() => {
    let active = true;
    listRooms().then(rooms => {
      if (!active) return;
      setHistory(rooms);
      setHistoryLoaded(true);
      if (rooms.length) {
        setRoom(rooms[0]);
        setScreen(rooms[0].phase === 'ready' ? 'allocation' : 'review');
      }
    }).catch(cause => { if (active) showError(cause); })
      .finally(() => { if (active) setInitialLoading(false); });
    return () => { active = false; };
  }, []);

  useEffect(() => { if (error) alert.current?.focus(); }, [error]);
  useEffect(() => { content.current?.focus({ preventScroll: true }); }, [screen]);

  useEffect(() => {
    setApproved(false);
    setQuote(null);
    setQuoteError('');
    if (!validBudget || screen !== 'allocation' || !room || !room.writable || room.phase !== 'ready' || imported) {
      setQuoteLoading(false);
      return;
    }
    let active = true;
    setQuoteLoading(true);
    const timer = window.setTimeout(() => {
      quoteAllocation({ budget: budgetNumber, leverage, side }).then(next => {
        if (active) setQuote(next);
      }).catch(cause => {
        if (active) setQuoteError(cause instanceof Error ? cause.message.slice(0, 300) : 'The quote is unavailable. Reload local history and try again.');
      }).finally(() => { if (active) setQuoteLoading(false); });
    }, 180);
    return () => { active = false; window.clearTimeout(timer); };
  }, [budgetNumber, validBudget, leverage, side, screen, room?.id, room?.phase, room?.headHash, room?.writable, imported]);

  function remember(next: RoomView) {
    setRoom(next);
    setImported(false);
    setHistory(previous => [next, ...previous.filter(item => item.id !== next.id)]);
    setHistoryLoaded(true);
  }

  async function action(label: string, run: () => Promise<void>) {
    if (pending.current) return;
    pending.current = true;
    setBusy(label);
    setError('');
    setNotice('');
    try { await run(); } catch (cause) { showError(cause); }
    finally { pending.current = false; setBusy(null); }
  }

  function recover() {
    void action('recovery', async () => {
      const rooms = await listRooms();
      setHistory(rooms);
      setHistoryLoaded(true);
      // Keep an unpublished draft on screen during recovery. A successful record
      // from an interrupted attempt remains available in the refreshed history.
      const recovered = room ? rooms.find(item => item.id === room.id) : undefined;
      if (recovered) {
        setRoom(recovered);
        setImported(false);
        setScreen(recovered.phase === 'ready' ? 'allocation' : 'review');
      }
      setNotice('Local history reloaded and verified. No new allocation was submitted. Open a saved record below if you need to resume it.');
    });
  }

  function startNewPlan() {
    if (pending.current) return;
    setRoom(null);
    setImported(false);
    setDraft(emptyPlan());
    setBudget('25');
    setLeverage(2);
    setSide('long');
    setApproved(false);
    setScreen('plan');
    setError('');
    setNotice('A new draft. Your previous records remain in local history.');
  }

  function selectRoom(next: RoomView) {
    if (pending.current) return;
    setRoom(next);
    setImported(false);
    setApproved(false);
    setError('');
    setNotice('');
    setScreen(next.phase === 'ready' ? 'allocation' : 'review');
  }

  function publish(event: React.FormEvent) {
    event.preventDefault();
    if (room) return;
    void action('publish', async () => {
      const next = await publishPlan({ title: draft.title.trim(), thesis: draft.thesis.trim(), invalidation: draft.invalidation.trim() });
      remember(next);
      setScreen('allocation');
      setNotice('Plan published locally. Its wording is now fixed in this record.');
    });
  }

  function approve(event: React.FormEvent) {
    event.preventDefault();
    if (!room || !room.writable || imported || room.phase !== 'ready' || !approved || !quote || !validBudget || quoteLoading) return;
    void action('approve', async () => {
      const next = await approveAllocation(room.id, allocation, room.headHash);
      remember(next);
      setScreen('review');
      setNotice('Allocation approved and recorded. Run the scenario to settle the paper position.');
    });
  }

  function simulate() {
    if (!room || !room.writable || imported || room.phase !== 'open') return;
    void action('scenario', async () => {
      remember(await runScenario(room.id, room.headHash));
      setNotice('The fixed scenario is complete. The result below comes from your verified paper record.');
    });
  }

  function download() {
    if (!room || imported) return;
    void action('export', async () => {
      const json = await exportRoom(room.id);
      const url = URL.createObjectURL(new Blob([json], { type: 'application/json' }));
      const link = document.createElement('a');
      link.href = url;
      link.download = `sakura-room-${room.id.replace(/[^a-zA-Z0-9_-]/g, '').slice(0, 80)}.json`;
      document.body.appendChild(link);
      link.click();
      link.remove();
      window.setTimeout(() => URL.revokeObjectURL(url), 1000);
      setNotice('Public record exported. It includes your plan and public identity, but no private signing keys.');
    });
  }

  function importProof(event: React.ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = '';
    if (!file) return;
    void action('verify', async () => {
      if (file.size > 280_000) throw new Error('This record is too large. Select a Rooms JSON export no larger than 280 KB.');
      const verified = await verifyExport(await file.text());
      setRoom(verified);
      setImported(true);
      setScreen('review');
      setNotice('Export verified. This is a read-only view; your local history has not been overwritten.');
    });
  }

  const committedQuote = room?.quote;
  const activeQuote = room?.phase === 'ready' ? quote : committedQuote;
  const pnl = room?.outcome?.netPnl;

  return <main className="perps-page rooms-page" data-theme={isDark ? 'dark' : 'light'}>
    <Head><title>Sakura Rooms — Local paper pilot</title><meta name="description" content="Write a plan, approve a paper allocation, and review a fixed scenario. A local Sakura Rooms experiment with no live trading or real funds." /></Head>
    <div className="rooms-container">
      <header className="rooms-header">
        <Link href="/perps" asChild><a className="perps-back"><span aria-hidden="true">桜</span> Sakura <span className="perps-breadcrumb">/</span> Rooms</a></Link>
        <div className="rooms-header-actions"><span className="rooms-local">Local paper pilot</span><button type="button" className="perps-theme" onClick={toggleTheme} aria-label={`Switch to ${isDark ? 'light' : 'dark'} mode`}><svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true">{isDark ? <><circle cx="12" cy="12" r="4" /><path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.5 1.5m11 11L19 19M5 19l1.5-1.5m11-11L19 5" /></> : <path d="M20 15.5A8.7 8.7 0 0 1 8.5 4 8.8 8.8 0 1 0 20 15.5Z" />}</svg></button></div>
      </header>
      <div className="rooms-heading"><div><p className="perps-eyebrow">A SMALL SPACE FOR A CLEARER PLAN</p><h1>Think it through.<br /><em>Then try it on paper.</em></h1><p>Put your reasoning on record. Choose your own limits. See what the scenario actually does to your allocation.</p></div><div className="rooms-meta"><strong>Personal practice session</strong>No live collaboration<br />No real funds</div></div>
      <nav className="rooms-nav" aria-label="Room steps">{screens.map((item, index) => <button key={item.id} type="button" aria-current={screen === item.id ? 'step' : undefined} disabled={locked || (item.id !== 'plan' && !room)} onClick={() => setScreen(item.id)}><span aria-hidden="true">0{index + 1}</span>{item.label}</button>)}</nav>
      {error ? <div className="rooms-notice" role="alert" tabIndex={-1} ref={alert}><strong>We could not complete that step.</strong>{error}<p>Your existing record and draft have not been intentionally reset. Reload local history to check whether the last action was recorded before retrying.</p><div className="rooms-actions"><button type="button" className="rooms-button" disabled={locked} onClick={recover}>{busy === 'recovery' ? 'Checking local history…' : 'Reload local history'}</button></div></div> : null}
      <div role="status" aria-live="polite">{initialLoading ? <p className="rooms-notice">Loading and verifying local history…</p> : notice ? <p className="rooms-notice">{notice}</p> : null}</div>
      {room?.recoveryNotice ? <div className="rooms-notice" role="status"><strong>Read-only recovery</strong>{room.recoveryNotice}<p>The verified record can still be reviewed and exported. Its signing identity will not be recreated.</p></div> : null}
      <div className="rooms-workspace">
        <section className="rooms-content" ref={content} tabIndex={-1} aria-label={screens.find(item => item.id === screen)?.label} aria-busy={Boolean(busy)}>
          {screen === 'plan' ? <>
            <h2 className="rooms-section-title">{room ? 'Your plan, on record.' : 'Start with the idea.'}</h2>
            <p className="rooms-description">{room ? 'This version is fixed. You can start a new plan without changing this record.' : 'A title, a thesis, and a reason to reconsider. You can edit everything here until you publish locally.'}</p>
            {room ? <><h3 className="rooms-section-title rooms-review-title" style={{ marginTop: 28 }}>{room.plan.title}</h3><PublishedPlan plan={room.plan} /><p className="rooms-note">The invalidation is a written reflection, not an automatic stop order. This scenario always settles at 104.</p><div className="rooms-actions"><button type="button" className="rooms-button rooms-button-primary" disabled={locked} onClick={() => setScreen(room.phase === 'ready' && room.writable && !imported ? 'allocation' : 'review')}>{room.phase === 'ready' && room.writable && !imported ? 'Choose my allocation' : 'View the record'}<Arrow /></button><button type="button" className="rooms-link-button" disabled={locked} onClick={startNewPlan}>Start a new plan</button></div></> : <form onSubmit={publish} className="rooms-form">
              <div className="rooms-field"><div className="rooms-field-head"><label htmlFor="rooms-title">Plan title</label><span>{draft.title.length}/80</span></div><input id="rooms-title" required maxLength={80} placeholder="Give this idea a name" value={draft.title} disabled={locked} onChange={event => setDraft(previous => ({ ...previous, title: event.target.value }))} /></div>
              <div className="rooms-field"><label htmlFor="rooms-thesis">My thesis</label><textarea id="rooms-thesis" required maxLength={1000} placeholder="What do I expect, and why?" value={draft.thesis} disabled={locked} onChange={event => setDraft(previous => ({ ...previous, thesis: event.target.value }))} /><small>Write your reasoning in your own words. The fixed scenario is visible beside this plan.</small></div>
              <div className="rooms-field"><label htmlFor="rooms-invalidation">What would change my mind?</label><textarea id="rooms-invalidation" required maxLength={500} placeholder="What evidence would make me reconsider?" value={draft.invalidation} disabled={locked} aria-describedby="rooms-invalidation-help" onChange={event => setDraft(previous => ({ ...previous, invalidation: event.target.value }))} /><small id="rooms-invalidation-help">A reflection prompt, not an executable stop order. The paper scenario always settles at 104.</small></div>
              <p className="rooms-note">Publishing fixes these words in your local record. It does not share them with other people. They will be included if you later export the public record.</p>
              <div className="rooms-actions" style={{ marginTop: 0 }}><button className="rooms-button rooms-button-primary" type="submit" disabled={locked || !draft.title.trim() || !draft.thesis.trim() || !draft.invalidation.trim()}>{busy === 'publish' ? 'Publishing locally…' : 'Publish plan locally'}<Arrow /></button><span className="rooms-note">No allocation yet.</span></div>
            </form>}
          </> : null}

          {screen === 'allocation' && room ? <>
            <h2 className="rooms-section-title">Your idea. Your allocation.</h2><p className="rooms-description">Choose a paper contribution of 1–100 credits for this plan. The budget includes the entry fee.</p>
            {room.phase !== 'ready' || imported || !room.writable ? <><div className="rooms-notice"><strong>{imported || !room.writable ? 'Read-only verified record' : 'Allocation already recorded'}</strong>{room.allocation ? `${room.allocation.side === 'long' ? 'Long' : 'Short'} SOL · ${room.allocation.leverage}× leverage. This allocation cannot be replaced.` : 'This published plan has no approved allocation.'}</div>{committedQuote ? <QuoteSummary quote={committedQuote} executed /> : null}<div className="rooms-actions"><button type="button" className="rooms-button rooms-button-primary" disabled={locked} onClick={() => setScreen('review')}>View review<Arrow /></button></div></> : <form className="rooms-form" onSubmit={approve}>
              <div className="rooms-field"><label htmlFor="rooms-budget">Total paper budget</label><input id="rooms-budget" type="number" inputMode="numeric" min={1} max={100} step={1} required value={budget} disabled={locked} aria-describedby="rooms-budget-help" aria-invalid={!validBudget} onChange={event => { setBudget(event.target.value); setApproved(false); }} /><small id="rooms-budget-help">1–100 whole credits, including the entry fee. {validBudget ? 'Any rounding remainder stays outside this position.' : 'Enter a whole number between 1 and 100.'}</small></div>
              <fieldset className="rooms-field rooms-fieldset" disabled={locked}><legend>Direction</legend><div className="rooms-choice-row" data-count="2">{(['long', 'short'] as const).map(value => <label className="rooms-choice" key={value}><input type="radio" name="rooms-side" value={value} checked={side === value} onChange={() => { setSide(value); setApproved(false); }} /><span>{value === 'long' ? 'Long · price rises' : 'Short · price falls'}</span></label>)}</div></fieldset>
              <fieldset className="rooms-field rooms-fieldset" disabled={locked}><legend>Leverage</legend><div className="rooms-choice-row">{([1, 2, 3] as const).map(value => <label className="rooms-choice" key={value}><input type="radio" name="rooms-leverage" value={value} checked={leverage === value} onChange={() => { setLeverage(value); setApproved(false); }} /><span>{value}×</span></label>)}</div><small>Higher leverage increases the size of gains and losses relative to your collateral.</small></fieldset>
              <div aria-live="polite">{quoteLoading ? <p className="rooms-description">Calculating the paper quote…</p> : quoteError ? <p className="rooms-notice" role="alert">{quoteError}</p> : quote ? <QuoteSummary quote={quote} /> : <p className="rooms-note">Enter a valid budget to see the quote.</p>}</div>
              <label className="rooms-approval"><input type="checkbox" checked={approved} disabled={locked || !quote || quoteLoading || !validBudget} onChange={event => setApproved(event.target.checked)} /><span>I approve this one paper allocation{quote ? ` of ${credits(quote.budget)}` : ''}, including its entry fee. I understand the plan is fixed and these are simulated credits.</span></label>
              <div className="rooms-actions" style={{ marginTop: 0 }}><button type="submit" className="rooms-button rooms-button-primary" disabled={locked || !approved || !quote || quoteLoading || !validBudget}>{busy === 'approve' ? 'Recording approval…' : 'Approve my allocation'}<Arrow /></button></div>
            </form>}
          </> : null}

          {screen === 'review' && room ? <>
            <p className="perps-eyebrow">{imported ? 'VERIFIED EXPORT · READ ONLY' : phaseLabels[room.phase].toUpperCase()}</p><h2 className="rooms-section-title rooms-review-title" style={{ marginTop: 12 }}>{room.plan.title}</h2><p className="rooms-description">Published {dateLabel(room.createdAt)}. The plan, approved terms, and result belong to this record.</p>
            <PublishedPlan plan={room.plan} />
            <p className="rooms-note">Your invalidation is a written reflection. It does not close the position during the dip to 92.</p>
            {room.outcome ? <div className="rooms-outcome"><div><p>Net result after entry fee</p><strong data-positive={!pnl?.startsWith('-')}>{pnl && !pnl.startsWith('-') && pnl !== '0' ? '+' : ''}{pnl ? formatCredits(pnl) : '0'} <span className="rooms-note">credits</span></strong><p>{room.allocation?.side === 'long' ? 'Long' : 'Short'} · {room.allocation?.leverage}× · settled at 104</p></div><div><p>Returned paper balance</p><strong>{formatCredits(room.outcome.endingBalance)} <span className="rooms-note">credits</span></strong><p>Position payout: {credits(room.outcome.payout)}</p></div></div> : <div className="rooms-notice"><strong>{room.phase === 'ready' ? 'No allocation approved yet.' : 'Your paper position is open.'}</strong>{room.phase === 'ready' ? 'Choose a budget and explicitly approve it to continue.' : 'Run the fixed 100 → 92 → 104 scenario to record settlement. No result has been generated yet.'}</div>}
            {activeQuote ? <><p className="rooms-description" style={{ marginBottom: 16 }}>{room.allocation?.side === 'long' ? 'Long' : 'Short'} SOL · {room.allocation?.leverage}× leverage</p><QuoteSummary quote={activeQuote} executed /></> : null}
            {!imported && room.writable && room.phase !== 'settled' ? <div className="rooms-actions"><button type="button" className="rooms-button rooms-button-primary" disabled={locked} onClick={room.phase === 'ready' ? () => setScreen('allocation') : simulate}>{busy === 'scenario' ? 'Running and verifying…' : room.phase === 'ready' ? 'Choose my allocation' : 'Run paper scenario'}<Arrow /></button></div> : null}
            <div className="rooms-notice"><strong className="rooms-status" data-ok={room.verified}>Receipt replay verified</strong>Signature and replay checks passed for {room.receiptCount} records. This verifies the local paper history, not market forecasts or production trading safety.</div>
            <details className="rooms-details"><summary>Record details and verification identity</summary><dl className="rooms-facts"><div><dt>Local room</dt><dd>{room.id}</dd></div><div><dt>Device paper identity</dt><dd>{room.publicKey.slice(0, 10)}…{room.publicKey.slice(-8)}</dd></div><div><dt>Recorded actions</dt><dd>{room.receiptCount}</dd></div></dl><p className="rooms-note">The public identity belongs to this paper record, not a connected trading wallet. The receipt head identifies the exact verified history.</p><pre aria-label="Receipt head hash">{room.headHash}</pre></details>
            <div className="rooms-actions">{!imported ? <button type="button" className="rooms-button" disabled={locked} onClick={download}>{busy === 'export' ? 'Preparing export…' : 'Export public record'}<Arrow /></button> : null}<button type="button" className="rooms-link-button" disabled={locked} onClick={startNewPlan}>Start a new plan</button></div>
            <p className="rooms-note" style={{ marginTop: 12 }}>An export contains the plan text, public identity, and signed receipts. It excludes private keys. Share it only if you want to share those details.</p>
          </> : null}
        </section>
        <ScenarioAside />
      </div>

      <section className="rooms-history" aria-labelledby="rooms-history-title"><div className="rooms-history-heading"><h2 id="rooms-history-title">Your local history</h2><button type="button" className="rooms-link-button" disabled={locked} onClick={recover}>{busy === 'recovery' ? 'Reloading…' : 'Reload & verify'}</button></div><p className="rooms-note">Records stay in this browser. Clearing its site data removes local records and paper signing identities.</p>
        {initialLoading ? <p className="rooms-description" style={{ marginTop: 16 }}>Loading records…</p> : !historyLoaded ? <p className="rooms-description" style={{ marginTop: 16 }}>History is unavailable. Check this browser’s storage and connection, then reload to recover your records.</p> : history.length === 0 ? <p className="rooms-description" style={{ marginTop: 16 }}>No published plans yet. Your first one will appear here.</p> : <ul>{history.map(item => <li key={item.id}><button type="button" disabled={locked} onClick={() => selectRoom(item)} aria-label={`Open ${item.plan.title}, ${phaseLabels[item.phase]}`}><span><strong>{item.plan.title}</strong><small>{dateLabel(item.createdAt)} · {phaseLabels[item.phase]}</small></span><Arrow /></button></li>)}</ul>}
        <div className="rooms-actions"><button type="button" className="rooms-button" disabled={locked} onClick={() => fileInput.current?.click()}>{busy === 'verify' ? 'Verifying export…' : 'Verify an exported record'}</button>{room ? <button type="button" className="rooms-link-button" disabled={locked} onClick={startNewPlan}>New plan</button> : null}</div><input ref={fileInput} type="file" accept="application/json,.json" onChange={importProof} hidden aria-label="Choose a public Rooms record" /><p className="rooms-note" style={{ marginTop: 10 }}>Verification opens a read-only view and does not replace local history.</p>
      </section>
      <footer className="rooms-footer"><p>Sakura Rooms · A local learning experiment.</p><p>Finite capped contracts. No live perpetuals, funding market, or real funds.</p></footer>
    </div>
  </main>;
}
