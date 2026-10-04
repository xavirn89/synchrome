<script>
  import { MorphIcon } from "morphicons/svelte";
  import { brandPaths, brandStroke } from "../lib/brand.js";
  import { iconNodes } from "../lib/iconography.js";

  let { name, size = 20, stroke = 1.8, morph = false, animate = false, class: className = "" } = $props();
  const nodes = $derived(iconNodes[name] ?? iconNodes.info);
</script>

{#if name === "logo"}
  <svg class={className} width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width={brandStroke} stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
    {#each brandPaths as path}<path d={path} />{/each}
  </svg>
{:else if morph}
  <span class="morph-icon">
    <MorphIcon icon={nodes} {size} strokeWidth={stroke} spring="snappy" reducedMotion={animate ? "user" : "always"} class={className} aria-hidden="true" focusable="false" />
  </span>
{:else}
  <svg class={className} width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width={stroke} stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
    {#each nodes as [tag, attributes]}<svelte:element this={tag} {...attributes} />{/each}
  </svg>
{/if}

<style>
  svg, .morph-icon { display: inline-block; flex-shrink: 0; vertical-align: middle; }
  .morph-icon { display: inline-flex; }
</style>
