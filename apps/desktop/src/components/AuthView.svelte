<script>
  import Icon from "./Icon.svelte";
  import { allowsIconMotion } from "../lib/iconography.js";

  let { busy = false, error = null, cloud = null, account = null, onsubmit = async () => {}, onretry = () => {} } = $props();
  const uid = $props.id();
  let mode = $state("signin");
  let email = $state("");
  let password = $state("");
  let showPassword = $state(false);
  let passwordIconMotion = $state(false);
  let submitting = $state(false);
  let submitted = $state(false);
  let emailTouched = $state(false);
  let passwordTouched = $state(false);
  let localError = $state(null);
  let confirmationDismissed = $state(false);

  const isSignup = $derived(mode === "signup");
  const isBusy = $derived(busy || submitting);
  const blocked = $derived(cloud?.configured === false || cloud?.auth_ok === false);
  const emailError = $derived((submitted || emailTouched) ? !email.trim() ? "Enter your email address." : !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim()) ? "Enter a valid email address." : "" : "");
  const passwordError = $derived((submitted || passwordTouched) ? !password ? "Enter your password." : isSignup && password.length < 8 ? "Use at least 8 characters for your password." : "" : "");
  const errorText = $derived(typeof error === "string" ? error : error?.text ?? "");
  const accountMessage = $derived(typeof account?.error === "string" ? account.error : "");
  const confirmationMessage = $derived(accountMessage.startsWith("Account created") ? accountMessage : errorText.startsWith("Account created") ? errorText : "");
  const awaitingConfirmation = $derived(!account?.signed_in && !!confirmationMessage && !confirmationDismissed);
  const visibleError = $derived(localError ?? (errorText && !errorText.startsWith("Account created") ? typeof error === "string" ? { text: error, detail: null } : error : null));

  function changeMode(next) {
    if (isBusy) return;
    mode = next;
    submitted = false;
    emailTouched = false;
    passwordTouched = false;
    localError = null;
    confirmationDismissed = true;
    showPassword = false;
  }

  async function submit(event) {
    event.preventDefault();
    if (isBusy || blocked) return;
    submitted = true;
    localError = null;
    if (emailError || passwordError) {
      document.getElementById(`${uid}-${emailError ? "email" : "password"}`)?.focus();
      return;
    }
    submitting = true;
    confirmationDismissed = false;
    try {
      const result = await onsubmit({ email: email.trim(), password, mode });
      if (result !== false && !error) password = "";
    } catch (cause) {
      localError = { text: "We couldn’t complete your request. Please try again.", detail: String(cause) };
    } finally {
      submitting = false;
      submitted = false;
      passwordTouched = false;
    }
  }
</script>

<main class="auth-view">
  <section class="auth-story" aria-label="About Synchrome">
    <div class="wordmark"><span class="brand-mark"><Icon name="logo" size={28} stroke={1.8} /></span><span>Synchrome</span></div>
    <div class="story-content">
      <h1>Your browsers,<br />in sync.</h1>
      <p class="story-intro">A home for your browser profiles. Bring your bookmarks, extensions and preferences across browsers and devices.</p>
      <div class="story-features">
        <div class="story-feature"><Icon name="profiles" size={20} /><div><strong>One profile. Every device.</strong><p>Keep work, personal and everything in between organised.</p></div></div>
        <div class="story-feature"><Icon name="browser" size={20} /><div><strong>Your browser, your choice.</strong><p>Bring Chrome or Helium together across your Macs.</p></div></div>
        <div class="story-feature"><Icon name="shield" size={20} /><div><strong>You choose what syncs.</strong><p>Control each connection and review changes when they need you.</p></div></div>
      </div>
    </div>
    <p class="story-footer">Made for the way you browse.</p>
  </section>

  <section class="auth-entry" aria-labelledby={`${uid}-heading`}>
    <div class="auth-form-wrap">
      {#if awaitingConfirmation}
        <div class="confirmation" role="status">
          <span class="confirmation-mark"><Icon name="check" size={26} /></span>
          <h2 id={`${uid}-heading`}>Check your inbox</h2>
          <p>Your account is ready to confirm. Open the confirmation link sent to <strong>{email || "your email address"}</strong>, then return here to sign in.</p>
          <button type="button" class="button primary" onclick={() => changeMode("signin")}><span>Back to sign in</span><Icon name="arrow-right" size={18} /></button>
        </div>
      {:else}
        <header class="auth-heading">
          <h2 id={`${uid}-heading`}>{isSignup ? "Create your account" : "Welcome back"}</h2>
          <p>{isSignup ? "Bring your browsers together with Synchrome." : "Sign in to your Synchrome workspace."}</p>
        </header>

        {#if blocked}
          <div class="connection-notice notice" role="status">
            <Icon name="warning" size={20} />
            <div><strong>{cloud?.configured === false ? "Cloud connection unavailable" : "Can’t reach the sign-in service"}</strong><p>{cloud?.configured === false ? "This copy of Synchrome needs its cloud connection configured before you can sign in." : "Check your internet connection and try again. Your browser data stays on this device."}</p><button type="button" class="connection-retry" onclick={onretry} disabled={isBusy}><Icon name="refresh" size={15} />Try again</button>{#if cloud?.error}<details><summary>Technical details</summary><code>{cloud.error}</code></details>{/if}</div>
          </div>
        {/if}

        <form class="login" novalidate aria-busy={isBusy} onsubmit={submit}>
          <div class="auth-field field">
            <label for={`${uid}-email`}>Email</label>
            <input id={`${uid}-email`} name="email" type="email" bind:value={email} autocomplete="username" inputmode="email" autocapitalize="none" spellcheck="false" required disabled={isBusy || blocked} aria-invalid={!!emailError} aria-describedby={emailError ? `${uid}-email-error` : undefined} onblur={() => { emailTouched = true; }} />
            {#if emailError}<span id={`${uid}-email-error`} class="validation" role="alert">{emailError}</span>{/if}
          </div>
          <div class="auth-field field">
            <label for={`${uid}-password`}>Password</label>
            <div class="password-input">
              <input id={`${uid}-password`} name="password" type={showPassword ? "text" : "password"} bind:value={password} autocomplete={isSignup ? "new-password" : "current-password"} required minlength={isSignup ? 8 : undefined} disabled={isBusy || blocked} aria-invalid={!!passwordError} aria-describedby={passwordError ? `${uid}-password-error` : isSignup ? `${uid}-password-help` : undefined} onblur={() => { passwordTouched = true; }} />
              <button type="button" class="password-toggle" aria-label={showPassword ? "Hide password" : "Show password"} aria-pressed={showPassword} disabled={isBusy || blocked} onclick={(event) => { passwordIconMotion = allowsIconMotion(event); showPassword = !showPassword; }}><Icon name={showPassword ? "eye-off" : "eye"} size={19} morph animate={passwordIconMotion} /></button>
            </div>
            {#if passwordError}<span id={`${uid}-password-error`} class="validation" role="alert">{passwordError}</span>{:else if isSignup}<span id={`${uid}-password-help`} class="field-hint">At least 8 characters.</span>{/if}
          </div>

          {#if visibleError}
            <div class="auth-error notice" role="alert"><Icon name="warning" size={19} /><div><p>{visibleError.text}</p>{#if visibleError.detail}<details><summary>Technical details</summary><code>{visibleError.detail}</code></details>{/if}</div></div>
          {/if}

          <button type="submit" class="button primary auth-submit" disabled={isBusy || blocked || !email.trim() || !password}>
            <span>{isBusy ? isSignup ? "Creating account…" : "Signing in…" : isSignup ? "Create account" : "Sign in"}</span>
            {#if !isBusy}<Icon name="arrow-right" size={18} />{/if}
          </button>
          <p class="auth-switch">{isSignup ? "Already have an account?" : "New to Synchrome?"}<button type="button" onclick={() => changeMode(isSignup ? "signin" : "signup")} disabled={isBusy}>{isSignup ? "Sign in" : "Create an account"}</button></p>
        </form>
      {/if}

      <div class="auth-privacy"><Icon name="shield" size={16} /><p>Your sign-in session is protected by macOS Keychain.</p></div>
      {#if cloud === null}<p class="cloud-check" role="status">Checking cloud connection…</p>{/if}
    </div>
  </section>
</main>

<style>
  .auth-view { display: grid; grid-template-columns: minmax(340px, 1fr) minmax(380px, 1fr); min-height: 100dvh; color: var(--fg); background: var(--surface); }
  .auth-story { display: flex; flex-direction: column; align-items: flex-start; padding: 48px 56px 32px; background: var(--bg); border-right: 1px solid var(--line); }
  .wordmark { display: flex; align-items: center; gap: 12px; font-size: 20px; font-weight: 650; letter-spacing: -0.025em; }
  .brand-mark { display: grid; place-items: center; width: 44px; height: 44px; border-radius: 12px; color: var(--accent); background: var(--accent-soft); }
  .story-content { width: min(100%, 440px); margin: auto 0; padding: 54px 0; }
  h1 { margin: 0; font-size: 42px; line-height: 1.13; letter-spacing: -0.035em; font-weight: 650; text-wrap: balance; }
  .story-intro { max-width: 40ch; margin: 22px 0 0; color: var(--muted); font-size: 16px; line-height: 1.7; }
  .story-features { display: grid; gap: 26px; margin-top: 42px; }
  .story-feature { display: flex; align-items: flex-start; gap: 13px; }
  .story-feature :global(svg) { margin-top: 2px; color: var(--muted); }
  .story-feature strong { font-size: 14px; font-weight: 600; }
  .story-feature p { max-width: 37ch; margin: 5px 0 0; color: var(--muted); font-size: 13px; line-height: 1.6; }
  .story-footer { margin: 0; color: var(--muted); font-size: 12px; }
  .auth-entry { display: grid; place-items: center; min-width: 0; padding: 48px 44px; }
  .auth-form-wrap { width: min(100%, 360px); }
  .auth-heading { margin-bottom: 32px; }
  h2 { margin: 0; font-size: 27px; font-weight: 650; letter-spacing: -0.03em; line-height: 1.25; }
  .auth-heading p { margin: 10px 0 0; color: var(--muted); font-size: 14px; line-height: 1.65; }
  .login { display: grid; gap: 20px; }
  .auth-field { display: grid; gap: 8px; min-width: 0; margin-bottom: 0; }
  label { font-size: 13px; font-weight: 600; }
  input { box-sizing: border-box; width: 100%; min-width: 0; height: 44px; padding: 0 13px; border: 1px solid var(--line); border-radius: 8px; background: var(--surface); color: var(--fg); font: inherit; font-size: 14px; caret-color: var(--accent); }
  input:focus { outline: 2px solid var(--accent); outline-offset: 2px; }
  input[aria-invalid="true"] { border-color: var(--danger); }
  input:disabled { background: var(--bg); color: var(--muted); opacity: 0.75; }
  .password-input { position: relative; }
  .password-input input { padding-right: 48px; }
  .password-toggle { position: absolute; top: 4px; right: 4px; display: grid; place-items: center; width: 36px; height: 36px; padding: 0; border: 0; border-radius: 5px; color: var(--muted); background: transparent; cursor: pointer; }
  .password-toggle:disabled { cursor: default; opacity: 0.4; }
  .password-toggle:focus-visible, .auth-switch button:focus-visible, .connection-retry:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  .validation { color: var(--danger); font-size: 12px; line-height: 1.5; }
  .field-hint { color: var(--muted); font-size: 12px; }
  .auth-submit { display: flex; align-items: center; justify-content: center; gap: 12px; min-height: 44px; margin-top: 4px; }
  .auth-switch { display: flex; flex-wrap: wrap; justify-content: center; align-items: center; gap: 4px 6px; margin: 2px 0 0; color: var(--muted); font-size: 13px; line-height: 1.5; }
  .auth-switch button { min-height: 32px; padding: 3px 0; border: 0; background: transparent; color: var(--accent); font: inherit; font-weight: 600; cursor: pointer; text-underline-offset: 4px; }
  .auth-switch button:disabled { color: var(--muted); opacity: 0.6; cursor: default; }
  .auth-privacy { display: flex; align-items: flex-start; justify-content: center; gap: 7px; margin-top: 36px; color: var(--muted); }
  .auth-privacy :global(svg) { margin-top: 1px; }
  .auth-privacy p { max-width: 34ch; margin: 0; font-size: 11px; line-height: 1.65; }
  .cloud-check { margin: 14px 0 0; text-align: center; color: var(--muted); font-size: 11px; }
  .connection-notice, .auth-error { display: flex; align-items: flex-start; gap: 10px; padding: 14px; border: 1px solid var(--line); border-radius: 10px; background: var(--bg); font-size: 13px; }
  .connection-notice { margin: -8px 0 24px; }
  .connection-notice > :global(svg) { margin-top: 1px; color: var(--warning); }
  .connection-notice strong { font-size: 13px; font-weight: 600; }
  .connection-notice p { margin: 6px 0 0; color: var(--muted); line-height: 1.6; }
  .connection-notice > div, .auth-error > div { min-width: 0; flex: 1; }
  .connection-retry { display: flex; align-items: center; gap: 6px; min-height: 32px; margin-top: 8px; padding: 2px 0; border: 0; color: var(--accent); background: transparent; font: inherit; font-weight: 600; cursor: pointer; }
  .auth-error { color: var(--danger); }
  .auth-error > :global(svg) { margin-top: 2px; }
  .auth-error p { margin: 0; line-height: 1.6; }
  details { margin-top: 10px; color: var(--muted); font-size: 11px; line-height: 1.6; }
  summary { cursor: pointer; text-underline-offset: 3px; }
  summary:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; border-radius: 3px; }
  code { display: block; margin-top: 8px; max-height: 140px; overflow: auto; font-size: 11px; white-space: pre-wrap; overflow-wrap: anywhere; }
  .confirmation-mark { display: grid; place-items: center; width: 52px; height: 52px; margin-bottom: 24px; border-radius: 14px; color: var(--accent); background: var(--accent-soft); }
  .confirmation p { margin: 14px 0 28px; color: var(--muted); font-size: 14px; line-height: 1.75; overflow-wrap: anywhere; }
  .confirmation strong { color: var(--fg); font-weight: 600; }
  .confirmation .button { display: flex; align-items: center; justify-content: center; gap: 12px; width: 100%; min-height: 44px; }
  @media (hover: hover) and (pointer: fine) {
    .password-toggle:hover:not(:disabled) { color: var(--fg); background: var(--bg); }
    .auth-switch button:hover:not(:disabled), .connection-retry:hover:not(:disabled) { text-decoration: underline; }
  }
  @media (max-width: 960px) {
    .auth-story { padding: 36px; }
    .auth-entry { padding: 40px 36px; }
    h1 { font-size: 36px; }
    .story-intro { font-size: 14px; }
    .story-features { gap: 22px; }
  }
  @media (max-width: 760px) {
    .auth-view { grid-template-columns: 1fr; grid-template-rows: auto 1fr; }
    .auth-story { padding: 26px 32px; border-right: 0; border-bottom: 1px solid var(--line); }
    .wordmark { font-size: 18px; }
    .brand-mark { width: 38px; height: 38px; border-radius: 10px; }
    .brand-mark :global(svg) { width: 24px; height: 24px; }
    .story-content { display: none; }
    .story-footer { display: none; }
    .auth-entry { padding: 42px 28px; align-items: start; }
  }
</style>
