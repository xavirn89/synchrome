<script>
  import { onDestroy } from "svelte";
  import Icon from "./Icon.svelte";

  let { open = false, title, description = "", onclose = () => {}, children, wide = false, dismissible = true } = $props();
  const uid = $props.id();
  let dialog = $state();
  let returnFocus;
  let backdropPress = false;

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) {
      returnFocus = document.activeElement;
      dialog.showModal();
    } else if (!open && dialog.open) {
      dialog.close();
    }
  });

  function restoreFocus() {
    if (returnFocus instanceof HTMLElement && returnFocus.isConnected) returnFocus.focus({ preventScroll: true });
    returnFocus = undefined;
  }

  function requestClose() {
    if (dismissible) onclose();
  }

  function cancel(event) {
    event.preventDefault();
    requestClose();
  }

  function outsideContent(event) {
    if (event.target !== dialog) return false;
    const rect = dialog.getBoundingClientRect();
    return event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom;
  }

  onDestroy(() => {
    if (dialog?.open) dialog.close();
    restoreFocus();
  });
</script>

<!-- The native dialog supplies modal semantics, inert siblings and focus containment. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<dialog bind:this={dialog} class:wide aria-labelledby={`${uid}-title`} aria-describedby={description ? `${uid}-description` : undefined} oncancel={cancel} onclose={restoreFocus} onpointerdown={(event) => { backdropPress = event.button === 0 && outsideContent(event); }} onpointerup={(event) => { if (event.button === 0 && backdropPress && outsideContent(event)) requestClose(); backdropPress = false; }}>
  <header class="dialog-header">
    <div class="dialog-heading">
      <h2 id={`${uid}-title`}>{title}</h2>
      {#if description}<p id={`${uid}-description`}>{description}</p>{/if}
    </div>
    <button type="button" class="dialog-close" aria-label="Close dialog" disabled={!dismissible} onclick={requestClose}><Icon name="close" size={19} /></button>
  </header>
  <div class="dialog-content">{@render children?.()}</div>
</dialog>

<style>
  dialog {
    box-sizing: border-box;
    width: min(480px, calc(100vw - 48px));
    max-height: calc(100dvh - 64px);
    margin: auto;
    padding: 0;
    border: 0;
    border-radius: 16px;
    background: var(--surface);
    color: var(--fg);
    box-shadow: 0 24px 72px rgb(0 0 0 / 24%);
    overflow: auto;
  }
  dialog.wide { width: min(720px, calc(100vw - 48px)); }
  dialog[open] { animation: dialog-enter 180ms cubic-bezier(0.16, 1, 0.3, 1); }
  dialog::backdrop { background: rgb(8 17 19 / 46%); }
  .dialog-header { display: flex; align-items: flex-start; gap: 24px; padding: 28px 28px 0; }
  .dialog-heading { min-width: 0; flex: 1; }
  h2 { margin: 0; font-size: 20px; line-height: 1.35; font-weight: 650; letter-spacing: -0.025em; overflow-wrap: anywhere; }
  p { margin: 9px 0 0; color: var(--muted); font-size: 14px; line-height: 1.6; overflow-wrap: anywhere; }
  .dialog-close { display: grid; place-items: center; flex-shrink: 0; width: 36px; height: 36px; margin: -5px -7px 0 0; padding: 0; border: 0; border-radius: 8px; color: var(--muted); background: transparent; cursor: pointer; }
  .dialog-close:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .dialog-close:active { background: var(--accent-soft); }
  .dialog-close:disabled { opacity: 0.4; cursor: default; }
  .dialog-content { padding: 24px 28px 28px; }
  @keyframes dialog-enter { from { opacity: 0; transform: translateY(6px); } to { opacity: 1; transform: translateY(0); } }
  @media (prefers-reduced-motion: reduce) { dialog[open] { animation: none; } }
  @media (hover: hover) and (pointer: fine) { .dialog-close:hover:not(:disabled) { background: var(--bg); color: var(--fg); } }
  @media (max-width: 540px) {
    dialog, dialog.wide { width: calc(100vw - 24px); max-height: calc(100dvh - 32px); }
    .dialog-header { padding: 22px 22px 0; }
    .dialog-content { padding: 22px; }
  }
</style>
