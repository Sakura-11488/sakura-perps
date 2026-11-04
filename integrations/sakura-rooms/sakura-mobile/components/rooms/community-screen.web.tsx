import React, { useEffect, useRef, useState } from 'react';
import { Link } from 'expo-router';
import Head from 'expo-router/head';
import { useTheme } from '@/lib/theme';
import { formatCredits, type AllocationInput, type AllocationQuote, type PlanDraft } from '@/lib/rooms-client.web';
import {
  connectCommunity, createCommunity, joinCommunity, refreshCommunity,
  createCommunityInvite, revokeCommunityInvite, revokeCommunityMember,
  quoteCommunityAllocation, approveCommunityAllocation, runCommunityScenario,
  recoverCommunityPending, exportCommunityRecord,
  type CommunitySession, type CommunityView,
} from '@/lib/rooms-community-client.web';
import '@/components/perps/perps-roadmap.web.css';
import './rooms.web.css';
import './community.web.css';

type Screen = 'plan' | 'allocation' | 'review';
type Revocation = { kind: 'member' | 'invite'; target: string };
const screens: { id: Screen; label: string }[] = [{ id: 'plan', label: 'Plan' }, { id: 'allocation', label: 'My allocation' }, { id: 'review', label: 'Review' }];
const emptyPlan = (): PlanDraft => ({ title: '', thesis: '', invalidation: '' });
const short = (value: string) => `${value.slice(0, 8)}…${value.slice(-6)}`;
const credits = (value: string) => `${formatCredits(value)} credits`;
const phaseLabel = { ready: 'No allocation yet', open: 'Allocation approved', settled: 'Scenario complete' };

function Arrow() { return <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><path d="M4 12h16m-6-6 6 6-6 6" /></svg>; }
function PublishedPlan({ plan }: { plan: PlanDraft }) {
  return <dl className="rooms-published-plan"><div><dt>Our thesis</dt><dd>{plan.thesis}</dd></div><div><dt>What would change our minds</dt><dd>{plan.invalidation}</dd></div></dl>;
}
function Quote({ quote }: { quote: AllocationQuote }) {
  return <section className="rooms-quote" aria-label="My paper allocation quote"><h3>Your allocation, including the fee</h3><dl className="rooms-facts">
    <div><dt>Position collateral</dt><dd>{credits(quote.collateral)}</dd></div><div><dt>Entry fee</dt><dd>{credits(quote.entryFee)}</dd></div>
    <div className="rooms-quote-total"><dt>Total paper budget</dt><dd>{credits(quote.budget)}</dd></div><div><dt>Position notional</dt><dd>{credits(quote.notional)}</dd></div>
    <div><dt>Profit cap</dt><dd>{credits(quote.profitCap)}</dd></div><div><dt>Maximum position payout</dt><dd>{credits(quote.maxPayout)}</dd></div><div><dt>Unspent remainder</dt><dd>{credits(quote.freeBalance)}</dd></div>
  </dl><p className="rooms-note">The maximum payout includes returned collateral. It is not guaranteed profit. This is your separate paper position; another member cannot spend its credits.</p></section>;
}

export default function CommunityRoomsScreen() {
  const { isDark, toggleTheme } = useTheme();
  const [session, setSession] = useState<CommunitySession | null>(null);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [screen, setScreen] = useState<Screen>('plan');
  const [draft, setDraft] = useState<PlanDraft>(emptyPlan);
  const [admission, setAdmission] = useState('');
  const [invite, setInvite] = useState('');
  const [joinConsent, setJoinConsent] = useState(false);
  const [createdInvite, setCreatedInvite] = useState<{ inviteUrl: string; inviteHash?: string } | null>(null);
  const [revocation, setRevocation] = useState<Revocation | null>(null);
  const [budget, setBudget] = useState('25');
  const [leverage, setLeverage] = useState<1 | 2 | 3>(2);
  const [side, setSide] = useState<'long' | 'short'>('long');
  const [quote, setQuote] = useState<AllocationQuote | null>(null);
  const [quoteLoading, setQuoteLoading] = useState(false);
  const [quoteError, setQuoteError] = useState('');
  const [approved, setApproved] = useState(false);
  const [initialLoading, setInitialLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [uncertain, setUncertain] = useState(false);
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const pendingAction = useRef(false);
  const alert = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLElement>(null);
  const revocationPanel = useRef<HTMLElement>(null);
  const room = session?.rooms.find(item => item.roomId === activeId) ?? null;
  const me = room?.members.find(member => member.publicKey === session?.publicKey);
  const activeCount = room?.members.filter(member => member.status === 'active').length ?? 0;
  const leader = Boolean(room && !room.ownOnly && session && room.leader === session.publicKey && session.writable);
  const pending = uncertain || Boolean(session?.pending);
  const locked = initialLoading || Boolean(busy);
  const cannotWrite = locked || pending || !session?.writable;
  const budgetNumber = Number(budget);
  const validBudget = /^\d+$/.test(budget) && Number.isInteger(budgetNumber) && budgetNumber >= 1 && budgetNumber <= 100;
  const canAllocate = Boolean(room && me?.status === 'active' && me.phase === 'ready' && session?.writable);
  const canSettle = Boolean(room && me?.phase === 'open' && session?.writable);
  const showError = (cause: unknown) => setError(cause instanceof Error ? cause.message.slice(0, 300) : 'This request could not be confirmed. Recover the saved request before approving another action.');

  useEffect(() => {
    let active = true;
    // A fragment stays out of HTTP requests and referrers. Merely opening an
    // invitation never joins or submits it; the visitor must confirm below.
    const hasInvitation = Boolean(window.location.hash);
    if (hasInvitation) {
      setInvite(window.location.href);
      window.history.replaceState(window.history.state, '', window.location.pathname + window.location.search);
    }
    connectCommunity().then(next => {
      if (!active) return;
      setSession(next);
      if (!hasInvitation && next.rooms.length) setActiveId(next.rooms[0].roomId);
    }).catch(cause => { if (active) showError(cause); }).finally(() => { if (active) setInitialLoading(false); });
    return () => { active = false; };
  }, []);
  useEffect(() => { if (error) alert.current?.focus(); }, [error]);
  useEffect(() => { content.current?.focus({ preventScroll: true }); }, [screen, activeId]);
  useEffect(() => { if (revocation) { revocationPanel.current?.focus({ preventScroll: true }); revocationPanel.current?.scrollIntoView({ block: 'center' }); } }, [revocation]);
  useEffect(() => {
    setApproved(false); setQuote(null); setQuoteError('');
    if (!canAllocate || !validBudget || screen !== 'allocation') { setQuoteLoading(false); return; }
    let active = true; setQuoteLoading(true);
    const timer = window.setTimeout(() => {
      quoteCommunityAllocation({ budget: budgetNumber, leverage, side }).then(next => { if (active) setQuote(next); })
        .catch(cause => { if (active) setQuoteError(cause instanceof Error ? cause.message.slice(0, 300) : 'The quote could not be verified.'); })
        .finally(() => { if (active) setQuoteLoading(false); });
    }, 180);
    return () => { active = false; window.clearTimeout(timer); };
  }, [canAllocate, validBudget, budgetNumber, leverage, side, screen, room?.roomId, room?.head]);

  function remember(next: CommunityView) {
    setSession(previous => previous ? { ...previous, rooms: [next, ...previous.rooms.filter(item => item.roomId !== next.roomId)], pending: false } : previous);
    setActiveId(next.roomId); setApproved(false); setRevocation(null);
  }
  async function action(label: string, run: () => Promise<void>, mutation = true) {
    if (pendingAction.current) return;
    pendingAction.current = true; setBusy(label); setError(''); setNotice('');
    try { await run(); } catch (cause) { if (mutation) setUncertain(true); showError(cause); }
    finally { pendingAction.current = false; setBusy(null); }
  }
  function recover() {
    void action('recover', async () => {
      const known = new Set(session?.rooms.map(item => item.roomId));
      const next = session ? await recoverCommunityPending() : await connectCommunity();
      setSession(next); setUncertain(false); setApproved(false);
      if (next.inviteUrl) setCreatedInvite({ inviteUrl: next.inviteUrl });
      if (!activeId && !invite.trim()) {
        const recovered = next.rooms.find(item => !known.has(item.roomId));
        if (recovered) { setActiveId(recovered.roomId); setAdmission(''); setInvite(''); setJoinConsent(false); }
      }
      setNotice(next.pending ? 'Connection and saved history verified. An earlier action still needs recovery before a new approval.' : 'Recovery finished. Review the verified state before approving a new action.');
    }, false);
  }
  function refresh() {
    if (!room) return;
    void action('refresh', async () => { remember(await refreshCommunity(room.roomId)); setNotice('Shared history refreshed and verified against this browser’s retained checkpoint.'); }, false);
  }
  function publish(event: React.FormEvent) {
    event.preventDefault();
    if (cannotWrite || room) return;
    void action('create', async () => {
      remember(await createCommunity(draft, admission.trim())); setAdmission(''); setCreatedInvite(null);
      setNotice('Your shared plan is published. It is fixed for everyone who joins. Create an invitation when you are ready.');
    });
  }
  function join(event: React.FormEvent) {
    event.preventDefault();
    if (cannotWrite || !joinConsent) return;
    void action('join', async () => {
      remember(await joinCommunity(invite.trim())); setInvite(''); setJoinConsent(false); setCreatedInvite(null); setScreen('plan');
      setNotice('You joined with your own paper identity. Read the shared plan before deciding on an allocation. Joining approves no allocation.');
    });
  }
  function newPlan() {
    if (locked || pending) return;
    setActiveId(null); setScreen('plan'); setDraft(emptyPlan()); setAdmission(''); setInvite(''); setJoinConsent(false); setCreatedInvite(null); setRevocation(null); setApproved(false); setError(''); setNotice('Your previous rooms remain in the history below.');
  }
  function select(next: CommunityView) {
    if (locked || pending) return;
    setActiveId(next.roomId); setScreen('plan'); setCreatedInvite(null); setRevocation(null); setApproved(false); setError(''); setNotice('Showing the last verified copy. Refresh shared history to request newer events.');
  }
  function issueInvite() {
    if (!room || !leader || cannotWrite) return;
    void action('invite', async () => {
      const result = await createCommunityInvite(room.roomId); remember(result.room); setCreatedInvite({ inviteHash: result.inviteHash, inviteUrl: result.inviteUrl });
      setNotice('A single-use invitation is ready. Share it privately with the person you want to join.');
    });
  }
  function revoke() {
    if (!room || !leader || !revocation || cannotWrite) return;
    const request = revocation;
    void action('revoke', async () => {
      const next = request.kind === 'member' ? await revokeCommunityMember(room.roomId, request.target) : await revokeCommunityInvite(room.roomId, request.target);
      remember(next);
      if (request.kind === 'invite' && createdInvite?.inviteHash === request.target) setCreatedInvite(null);
      setNotice(request.kind === 'member' ? 'Membership revoked. The member cannot make a new allocation; an existing paper position can still be completed and exported.' : 'Invitation revoked. It can no longer be used to join.');
    });
  }
  function approve(event: React.FormEvent) {
    event.preventDefault();
    if (!room || !canAllocate || cannotWrite || !approved || !quote || quoteLoading || !validBudget) return;
    const input: AllocationInput = { budget: budgetNumber, leverage, side };
    void action('approve', async () => { remember(await approveCommunityAllocation(room.roomId, input, room.head)); setScreen('review'); setNotice('Your own allocation was signed, saved and verified. Other members can now review it.'); });
  }
  function simulate() {
    if (!room || !canSettle || cannotWrite) return;
    void action('scenario', async () => { remember(await runCommunityScenario(room.roomId, room.head)); setNotice('Your fixed paper scenario is complete. The verified result includes your entry fee.'); });
  }
  function download() {
    if (!room) return;
    void action('export', async () => {
      const data = await exportCommunityRecord(room.roomId);
      const url = URL.createObjectURL(new Blob([data], { type: 'application/json' }));
      const anchor = document.createElement('a'); anchor.href = url; anchor.download = `sakura-community-${room.roomId.slice(0, 8)}-my-record.json`; anchor.click();
      window.setTimeout(() => URL.revokeObjectURL(url), 1_000);
      setNotice(room.ownOnly ? 'Your own paper record was exported without a private signing key. It cannot restore signing access.' : 'The verified room record was exported, including its shared plan, member identities and paper actions. Share it only with the participants’ agreement. It contains no private signing key.');
    }, false);
  }
  function copyInvite() {
    if (!createdInvite) return;
    void action('copy', async () => { await navigator.clipboard.writeText(createdInvite.inviteUrl); setNotice('Invitation copied. Share it privately; anyone holding this unused link can claim its one place.'); }, false);
  }

  return <main className="perps-page rooms-page community-page" data-theme={isDark ? 'dark' : 'light'}>
    <Head><title>Sakura Rooms — Community paper beta</title><meta name="description" content="A shared plan, separate paper allocations, and verified community history. Sakura Rooms invite-only beta." /><meta name="referrer" content="no-referrer" /></Head>
    <div className="rooms-container">
      <header className="rooms-header"><Link href="/perps/rooms" asChild><a className="perps-back"><span aria-hidden="true">桜</span> Sakura <span className="perps-breadcrumb">/</span> Community Rooms</a></Link><div className="rooms-header-actions"><span className="rooms-local">Invite-only paper beta</span><button type="button" className="perps-theme" onClick={toggleTheme} aria-label={`Switch to ${isDark ? 'light' : 'dark'} mode`}><svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true"><circle cx="12" cy="12" r="4" /><path d="M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1.5 1.5m11 11L19 19M5 19l1.5-1.5m11-11L19 5" /></svg></button></div></header>
      <div className="rooms-heading"><div><p className="perps-eyebrow">MAKE SPACE FOR ANOTHER PERSPECTIVE</p><h1>A shared idea.<br /><em>Your own say.</em></h1><p>Think together. Choose your own allocation. Put the plan, each approval, and what happened on one verifiable record.</p></div><div className="rooms-meta"><strong>{room ? `${activeCount} active ${activeCount === 1 ? 'participant' : 'participants'}` : 'By invitation'}</strong>Separate paper identities<br />No real funds</div></div>
      <nav className="rooms-nav" aria-label="Community room steps">{screens.map((item, index) => <button key={item.id} type="button" aria-current={screen === item.id ? 'step' : undefined} disabled={locked || (item.id !== 'plan' && !room)} onClick={() => setScreen(item.id)}><span aria-hidden="true">0{index + 1}</span>{item.label}</button>)}</nav>
      {error ? <div className="rooms-notice" role="alert" tabIndex={-1} ref={alert}><strong>This step could not be confirmed.</strong>{error}<div className="rooms-actions"><button type="button" className="rooms-button" disabled={locked} onClick={recover}>Recover saved request</button></div></div> : null}
      <div role="status" aria-live="polite">{initialLoading ? <p className="rooms-notice">Connecting and verifying saved community history…</p> : notice ? <p className="rooms-notice">{notice}</p> : null}</div>
      {pending ? <div className="rooms-notice"><strong>Finish recovery before a new approval.</strong>An earlier request may already have been saved. Recovery checks that same request; it does not authorize a different allocation.<div className="rooms-actions"><button type="button" className="rooms-button" disabled={locked} onClick={recover}>Recover pending action</button></div></div> : null}
      {session?.recoveryNotice ? <div className="rooms-notice"><strong>Paper identity recovery</strong>{session.recoveryNotice}</div> : null}
      {room?.ownOnly ? <div className="rooms-notice"><strong>Membership revoked · your record remains available</strong>You can complete an existing paper allocation and export your own record. Shared-history access and new allocations are no longer available. The plan is the version you previously accepted.</div> : null}
      <div className="rooms-workspace">
        <section className="rooms-content" ref={content} tabIndex={-1} aria-label={screens.find(item => item.id === screen)?.label} aria-busy={Boolean(busy)}>
          {screen === 'plan' && !room ? <>
            <section className="community-join" aria-labelledby="community-join-title"><p className="perps-eyebrow">ALREADY INVITED?</p><h2 id="community-join-title" className="rooms-section-title">Take your seat.</h2><p className="rooms-description">Join with a private invitation. You will see the fixed plan before choosing whether to allocate any paper credits.</p><form className="rooms-form" onSubmit={join}>
              <div className="rooms-field"><label htmlFor="community-invite">Private invitation link</label><input id="community-invite" type="password" autoComplete="off" spellCheck={false} value={invite} maxLength={2048} disabled={cannotWrite} onChange={event => { setInvite(event.target.value); setJoinConsent(false); }} /><small>The invitation stays out of the page address when opened. Joining shares your paper identity with this room.</small></div>
              <label className="rooms-approval"><input type="checkbox" checked={joinConsent} disabled={cannotWrite || !invite.trim()} onChange={event => setJoinConsent(event.target.checked)} /><span>I want to join this private paper room. My identity and any allocation I later approve will be visible to its members.</span></label>
              <div className="rooms-actions" style={{ marginTop: 0 }}><button className="rooms-button rooms-button-primary" type="submit" disabled={cannotWrite || !invite.trim() || !joinConsent}>{busy === 'join' ? 'Joining…' : 'Join invited room'}<Arrow /></button></div>
            </form></section>
            <section className="community-create" aria-labelledby="community-create-title"><p className="perps-eyebrow">HAVE A HOST ADMISSION?</p><h2 id="community-create-title" className="rooms-section-title">Start with the idea.</h2><p className="rooms-description">Publish one plan, then invite people to explore it. Every member signs their own allocation. Hosting does not give you authority over theirs.</p><form onSubmit={publish} className="rooms-form">
              <div className="rooms-field"><label htmlFor="community-admission">Host admission code</label><input id="community-admission" type="password" required autoComplete="off" spellCheck={false} maxLength={64} value={admission} disabled={cannotWrite} onChange={event => setAdmission(event.target.value)} /><small>A single-use code supplied by the beta operator. Creating a room consumes it.</small></div>
              <div className="rooms-field"><div className="rooms-field-head"><label htmlFor="community-title">Plan title</label><span>{draft.title.length}/80</span></div><input id="community-title" required maxLength={80} placeholder="Give the shared idea a name" value={draft.title} disabled={cannotWrite} onChange={event => setDraft(previous => ({ ...previous, title: event.target.value }))} /></div>
              <div className="rooms-field"><label htmlFor="community-thesis">Our thesis</label><textarea id="community-thesis" required maxLength={1000} value={draft.thesis} disabled={cannotWrite} onChange={event => setDraft(previous => ({ ...previous, thesis: event.target.value }))} /></div>
              <div className="rooms-field"><label htmlFor="community-invalidation">What would change our minds?</label><textarea id="community-invalidation" required maxLength={500} value={draft.invalidation} disabled={cannotWrite} onChange={event => setDraft(previous => ({ ...previous, invalidation: event.target.value }))} /><small>A reflection prompt, not an automatic stop order. The known paper scenario settles at 104.</small></div>
              <p className="rooms-note">Publishing fixes this plan and shares it with the service and invited members. Keep personal or confidential information out of the plan.</p>
              <div className="rooms-actions" style={{ marginTop: 0 }}><button type="submit" className="rooms-button rooms-button-primary" disabled={cannotWrite || !admission.trim() || !draft.title.trim() || !draft.thesis.trim() || !draft.invalidation.trim()}>{busy === 'create' ? 'Publishing…' : 'Publish shared plan'}<Arrow /></button></div>
            </form></section>
          </> : null}
          {screen === 'plan' && room ? <>
            <p className="perps-eyebrow">A FIXED PLAN · INDIVIDUAL APPROVALS</p><h2 className="rooms-section-title rooms-review-title" style={{ marginTop: 12 }}>{room.plan.title}</h2><p className="rooms-description">{room.leader === session?.publicKey ? 'You host this room.' : `Hosted by ${short(room.leader)}.`} The published wording is fixed for every member.</p><PublishedPlan plan={room.plan} /><p className="rooms-note">This invalidation is a written reflection. It does not close a position during the illustrated dip to 92.</p>
            <div className="rooms-actions"><button type="button" className="rooms-button rooms-button-primary" disabled={locked} onClick={() => setScreen(me?.phase === 'ready' ? 'allocation' : 'review')}>{me?.phase === 'ready' ? 'Choose my allocation' : 'View our review'}<Arrow /></button><button type="button" className="rooms-link-button" disabled={locked || pending} onClick={refresh}>Refresh shared history</button></div>
            {leader ? <section className="community-invites" aria-labelledby="community-invite-heading"><h3 id="community-invite-heading">Make room for another perspective.</h3><p className="rooms-note">Up to eight identities can join. Each invitation can be claimed once; share it privately.</p><div className="rooms-actions"><button type="button" className="rooms-button" disabled={cannotWrite || room.members.length >= 8 || room.invites.length >= 64} onClick={issueInvite}>{busy === 'invite' ? 'Creating invitation…' : 'Create private invitation'}</button></div>
              {createdInvite ? <div className="community-invite-result"><div className="rooms-field"><label htmlFor="community-created-invite">New private invitation</label><input id="community-created-invite" readOnly type="password" autoComplete="off" value={createdInvite.inviteUrl} /><small>This secret link is shown only in this session. If you lose it, revoke the unused invitation and create another.</small></div><div className="rooms-actions"><button type="button" className="rooms-button" disabled={locked} onClick={copyInvite}>Copy invitation</button>{createdInvite.inviteHash ? <button type="button" className="rooms-link-button" disabled={cannotWrite} onClick={() => setRevocation({ kind: 'invite', target: createdInvite.inviteHash! })}>Revoke this invitation</button> : null}</div></div> : null}
              {room.invites.length ? <ul className="community-invite-list" aria-label="Room invitations">{room.invites.map(item => <li key={item.inviteHash}><span><strong>{short(item.inviteHash)}</strong><small>{item.status === 'claimed' ? 'Claimed' : item.status === 'revoked' ? 'Revoked' : 'Unused'}</small></span>{item.status === 'active' ? <button type="button" className="rooms-link-button" disabled={cannotWrite} onClick={() => setRevocation({ kind: 'invite', target: item.inviteHash })} aria-label={`Revoke invitation ${short(item.inviteHash)}`}>Revoke</button> : null}</li>)}</ul> : null}
            </section> : null}
          </> : null}
          {screen === 'allocation' && room ? <>
            <h2 className="rooms-section-title">One plan. Your own limits.</h2><p className="rooms-description">Only your paper identity can approve this allocation. The host and other members cannot enlarge it or choose it for you.</p>
            {!canAllocate ? <><div className="rooms-notice"><strong>{me?.status === 'revoked' ? 'Your membership was revoked' : !session?.writable ? 'Read-only paper history' : 'Your allocation is on record'}</strong>{me?.allocation ? `${me.allocation.side === 'long' ? 'Long' : 'Short'} SOL · ${me.allocation.leverage}× leverage. This allocation cannot be replaced.` : 'No new allocation can be approved from this view.'}</div>{me?.quote ? <Quote quote={me.quote} /> : null}<div className="rooms-actions"><button type="button" className="rooms-button" disabled={locked} onClick={() => setScreen('review')}>View our review<Arrow /></button></div></> : <form className="rooms-form" onSubmit={approve}>
              <div className="rooms-field"><label htmlFor="community-budget">My total paper budget</label><input id="community-budget" type="number" min={1} max={100} step={1} inputMode="numeric" required value={budget} disabled={cannotWrite} aria-invalid={!validBudget} aria-describedby="community-budget-help" onChange={event => { setBudget(event.target.value); setApproved(false); }} /><small id="community-budget-help">1–100 whole credits including the entry fee. The rounding remainder stays outside your position.</small></div>
              <fieldset className="rooms-field rooms-fieldset" disabled={cannotWrite}><legend>My direction</legend><div className="rooms-choice-row" data-count="2">{(['long', 'short'] as const).map(value => <label className="rooms-choice" key={value}><input type="radio" name="community-side" value={value} checked={side === value} onChange={() => { setSide(value); setApproved(false); }} /><span>{value === 'long' ? 'Long · price rises' : 'Short · price falls'}</span></label>)}</div></fieldset>
              <fieldset className="rooms-field rooms-fieldset" disabled={cannotWrite}><legend>My leverage</legend><div className="rooms-choice-row">{([1, 2, 3] as const).map(value => <label className="rooms-choice" key={value}><input type="radio" name="community-leverage" value={value} checked={leverage === value} onChange={() => { setLeverage(value); setApproved(false); }} /><span>{value}×</span></label>)}</div></fieldset>
              <div aria-live="polite">{quoteLoading ? <p className="rooms-description">Verifying your paper quote…</p> : quoteError ? <p className="rooms-notice" role="alert">{quoteError}</p> : quote ? <Quote quote={quote} /> : <p className="rooms-note">Enter a valid budget to see your quote.</p>}</div>
              <label className="rooms-approval"><input type="checkbox" checked={approved} disabled={cannotWrite || !quote || quoteLoading || !validBudget} onChange={event => setApproved(event.target.checked)} /><span>I approve my own paper allocation{quote ? ` of ${credits(quote.budget)}` : ''}, including its entry fee, for this fixed shared plan. Its terms and result will be visible to room members.</span></label>
              <div className="rooms-actions" style={{ marginTop: 0 }}><button type="submit" className="rooms-button rooms-button-primary" disabled={cannotWrite || !approved || !quote || quoteLoading || !validBudget}>{busy === 'approve' ? 'Recording my approval…' : 'Approve my allocation'}<Arrow /></button></div>
            </form>}
          </> : null}
          {screen === 'review' && room ? <>
            <p className="perps-eyebrow">COMPARE THE CHOICES, THEN THE RESULT</p><h2 className="rooms-section-title rooms-review-title" style={{ marginTop: 12 }}>{room.plan.title}</h2><p className="rooms-description">Everyone sees the same fixed scenario. Each allocation and outcome belongs to its approving identity.</p>
            {me?.outcome ? <section className="rooms-outcome" aria-label="My verified paper result"><div><p>My net result after entry fee</p><strong data-positive={!me.outcome.netPnl.startsWith('-')}>{!me.outcome.netPnl.startsWith('-') && me.outcome.netPnl !== '0' ? '+' : ''}{formatCredits(me.outcome.netPnl)} <span className="rooms-note">credits</span></strong></div><div><p>My ending paper balance</p><strong>{formatCredits(me.outcome.endingBalance)} <span className="rooms-note">credits</span></strong></div></section> : <div className="rooms-notice"><strong>{me?.phase === 'open' ? 'Your allocation is waiting for its result.' : 'You have not approved an allocation.'}</strong>{me?.phase === 'open' ? 'Run your fixed scenario to record its final settlement. No result has been generated for you yet.' : 'You can review the shared plan without allocating paper credits.'}</div>}
            <div className="rooms-actions">{canSettle ? <button type="button" className="rooms-button rooms-button-primary" disabled={cannotWrite} onClick={simulate}>{busy === 'scenario' ? 'Recording my result…' : 'Run my paper scenario'}<Arrow /></button> : canAllocate ? <button type="button" className="rooms-button rooms-button-primary" disabled={cannotWrite} onClick={() => setScreen('allocation')}>Choose my allocation<Arrow /></button> : null}<button type="button" className="rooms-button" disabled={locked} onClick={download}>Export my public record</button><button type="button" className="rooms-link-button" disabled={locked || pending} onClick={refresh}>Refresh shared history</button></div>
            <p className="rooms-note" style={{ marginTop: 20 }}>These known prices test the workflow and your chosen limits, not trading skill. A positive result is not proof of a repeatable strategy.</p><PublishedPlan plan={room.plan} />
          </> : null}
          {revocation && leader ? <section className="rooms-notice" aria-label="Confirm access revocation" ref={revocationPanel} tabIndex={-1}><strong>{revocation.kind === 'member' ? 'Revoke this membership?' : 'Revoke this unused invitation?'}</strong><p>{short(revocation.target)}. {revocation.kind === 'member' ? 'This blocks new allocations and shared-history access. It preserves the member’s own paper record and ability to complete an existing allocation.' : 'The invitation will stop admitting new members. A claimed membership must be revoked separately.'}</p><div className="rooms-actions"><button type="button" className="rooms-button" disabled={cannotWrite} onClick={revoke}>Confirm revocation</button><button type="button" className="rooms-link-button" disabled={locked} onClick={() => setRevocation(null)}>Keep access</button></div></section> : null}
        </section>
        <aside className="rooms-aside community-aside" aria-label="Paper scenario and identity"><p className="perps-eyebrow">THE SHARED PRACTICE</p><h2>Same scenario.<br />Separate decisions.</h2><div className="community-price-path" aria-label="Fixed SOL paper scenario: opens at 100, indicative dip to 92, settles at 104"><span><strong>100</strong><small>Open</small></span><span aria-hidden="true">↘</span><span><strong>92</strong><small>Illustrative dip</small></span><span aria-hidden="true">↗</span><span><strong>104</strong><small>Settle</small></span></div><p className="rooms-note">The midpoint is illustrative. It does not execute a stop or liquidation. Each finite paper position has a capped payout.</p><dl className="rooms-facts"><div><dt>My contribution limit</dt><dd>100 paper credits</dd></div><div><dt>Allocations per member</dt><dd>One</dd></div><div><dt>Member capacity</dt><dd>Eight identities</dd></div><div><dt>Real funds</dt><dd>None</dd></div></dl>
          <div className="community-identity"><p className="perps-eyebrow">MY PAPER IDENTITY</p><strong data-testid="community-identity">{session ? short(session.publicKey) : 'Not connected'}</strong><p className="rooms-note">Your signing key stays in this browser. It is separate from any trading wallet. Clearing site data can remove your signing access.</p></div>
          {session ? <details className="rooms-details"><summary>Service and identity details</summary><p className="rooms-note">This browser retains the service identity and verified history checkpoints. A complete loss of those local checkpoints removes that rollback evidence.</p><dl className="rooms-facts"><div><dt>My public identity</dt><dd data-testid="community-public-key">{session.publicKey}</dd></div><div><dt>Service identity</dt><dd>{session.serviceId}</dd></div></dl></details> : <p className="rooms-note">The invite-only service must be available before joining or publishing a room.</p>}
        </aside>
      </div>
      {room ? <section className="community-members" aria-labelledby="community-members-title"><div className="rooms-history-heading"><div><p className="perps-eyebrow">AROUND THIS TABLE</p><h2 id="community-members-title">Our choices, on record.</h2></div><span className="rooms-status" data-ok="true">{room.ownOnly ? 'My verified paper record' : `Verified history · ${room.eventCount} events`}</span></div><p className="rooms-description">{room.ownOnly ? 'Showing only your own verified record after membership revocation.' : 'A last verified snapshot. Refresh to see newly recorded approvals and results. There is no shared balance or automatic copying.'}</p><ul className="community-member-list">{room.members.filter(member => !room.ownOnly || member.publicKey === session?.publicKey).map(member => <li key={member.publicKey} data-member={member.publicKey}><div className="community-member-name"><strong>{member.publicKey === session?.publicKey ? 'You' : short(member.publicKey)}</strong><span>{member.publicKey === room.leader ? 'Host' : 'Member'} · {member.status === 'revoked' ? 'Access revoked' : 'Active'}</span></div><div><small>Allocation</small><strong>{member.allocation ? `${member.allocation.budget} credits · ${member.allocation.side} · ${member.allocation.leverage}×` : 'Not approved'}</strong><span>{phaseLabel[member.phase]}</span></div><div><small>Net result after fee</small><strong className={member.outcome ? 'community-result' : ''}>{member.outcome ? `${!member.outcome.netPnl.startsWith('-') && member.outcome.netPnl !== '0' ? '+' : ''}${formatCredits(member.outcome.netPnl)} credits` : 'Awaiting result'}</strong></div>{leader && member.publicKey !== room.leader && member.status === 'active' ? <button type="button" className="rooms-link-button" disabled={cannotWrite} onClick={() => setRevocation({ kind: 'member', target: member.publicKey })} aria-label={`Revoke membership ${short(member.publicKey)}`}>Revoke access</button> : null}</li>)}</ul><details className="rooms-details"><summary>{room.ownOnly ? 'Last retained shared checkpoint' : 'Shared record checkpoint'}</summary><dl className="rooms-facts"><div><dt>Room identity</dt><dd data-testid="community-room-id">{room.roomId}</dd></div><div><dt>Verified events</dt><dd>{room.eventCount}</dd></div></dl><pre aria-label="Shared history head">{room.head}</pre><p className="rooms-note">{room.ownOnly ? 'This is the shared checkpoint retained before access ended, not a current shared-history view. Your own paper record is verified separately.' : 'A checkpoint identifies this exact shared history. It does not guarantee that a server has never shown another history to another browser.'}</p></details></section> : null}
      <section className="rooms-history community-history" aria-labelledby="community-history-title"><div className="rooms-history-heading"><h2 id="community-history-title">My community rooms</h2><button type="button" className="rooms-link-button" disabled={locked} onClick={recover}>Recover &amp; verify</button></div><p className="rooms-note">The service retains shared history. This browser retains your identity, pending requests and checkpoints. Public exports cannot restore a lost private key.</p>{session?.rooms.length ? <ul>{session.rooms.map(item => <li key={item.roomId}><button type="button" disabled={locked || pending} onClick={() => select(item)} aria-label={`Open community room ${item.plan.title}`}><span><strong>{item.plan.title}</strong><small>{item.members.length} recorded participants · {item.eventCount} verified events</small></span><Arrow /></button></li>)}</ul> : <p className="rooms-description" style={{ marginTop: 20 }}>{initialLoading ? 'Loading saved rooms…' : session ? 'No community rooms on this device yet.' : 'Connect to the beta service to recover your saved rooms.'}</p>}{room ? <div className="rooms-actions"><button type="button" className="rooms-button" disabled={cannotWrite} onClick={newPlan}>Join or start another room</button></div> : null}</section>
      <footer className="rooms-footer"><p>Sakura Rooms · Practice together. Approve for yourself.</p><p>Invite-only paper beta. No deposits, live orders, or real funds.</p></footer>
    </div>
  </main>;
}
