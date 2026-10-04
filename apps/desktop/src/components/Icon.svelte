<script>
  import { brandPaths, brandStroke } from "../lib/brand.js";
  let { name, size = 20, stroke = 1.8, class: className = "" } = $props();

  // Original geometric paths. One grid and stroke keep the interface consistent.
  const paths = {
    logo: brandPaths,
    overview: ["M3 3h7v7H3z", "M14 3h7v7h-7z", "M3 14h7v7H3z", "M14 14h7v7h-7z"],
    profiles: ["M3 5h7l2 3h9v11H3z", "M3 8h9"],
    devices: ["M3 4h13v11H3z", "M6 19h7", "M9.5 15v4", "M19 9h3v12h-5v-6"],
    activity: ["M3 12h4l3-8 4 16 3-8h4"],
    settings: ["M4 6h16", "M4 12h16", "M4 18h16", "M8 4v4", "M16 10v4", "M10 16v4"],
    plus: ["M12 5v14", "M5 12h14"],
    "arrow-right": ["M4 12h16", "m14 6 6 6-6 6"],
    "arrow-left": ["M20 12H4", "m10 6-6 6 6 6"],
    left: ["M20 12H4", "m10 6-6 6 6 6"],
    check: ["m5 12 4 4L19 6"],
    close: ["m6 6 12 12", "M18 6 6 18"],
    "chevron-down": ["m6 9 6 6 6-6"],
    "chevron-right": ["m9 6 6 6-6 6"],
    cloud: ["M7 18h11a4 4 0 0 0 .6-7.95A6.5 6.5 0 0 0 6 9.2 4.5 4.5 0 0 0 7 18Z"],
    refresh: ["M20 7v5h-5", "M4 17v-5h5", "M5.5 7a7 7 0 0 1 12-1L20 9", "M4 15l2.5 3a7 7 0 0 0 12-1"],
    laptop: ["M5 4h14v12H5z", "M2 16h20l-2 4H4z"],
    browser: ["M3 4h18v16H3z", "M3 9h18", "M6 6.5h.01", "M9 6.5h.01"],
    bookmark: ["M6 3h12v18l-6-4-6 4z"],
    extension: ["M9 3h6v4a3 3 0 1 1 4 4h2v10h-6v-3a3 3 0 1 0-6 0v3H3v-6h3a3 3 0 1 0 0-6H3V7h6z"],
    shield: ["m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6z", "m8 12 3 3 5-6"],
    warning: ["m12 3 10 18H2z", "M12 9v5", "M12 17h.01"],
    info: ["M12 8h.01", "M11 11h1v6", "M10 17h4"],
    moon: ["M20.8 13.2A9 9 0 0 1 10.8 3.2a9 9 0 1 0 10 10Z"],
    sun: ["M12 2v2", "M12 20v2", "M2 12h2", "M20 12h2", "m5 5 1.5 1.5", "M17.5 17.5 19 19", "m5 19 1.5-1.5", "M17.5 6.5 19 5"],
    system: ["M3 4h18v13H3z", "M8 21h8", "M12 17v4"],
    logout: ["M9 4H4v16h5", "M9 12h12", "m16 7 5 5-5 5"],
    trash: ["M3 6h18", "M5 6l1 15h12l1-15", "M9 6V3h6v3", "M10 10v7", "M14 10v7"],
    edit: ["m4 16-1 5 5-1L20 8l-4-4Z", "m13 7 4 4"],
    link: ["m9 15 6-6", "M8 13 5 16a3 3 0 0 0 4 4l3-3", "m12 7 3-3a3 3 0 0 1 4 4l-3 3"],
    unplug: ["m7 7 10 10", "m15 3-3 3", "m21 9-3 3", "m12 6 6 6", "m6 12 6 6-2 2H7l-3-3v-3z", "m2 22 3-3"],
    eye: ["M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7Z"],
    "eye-off": ["M3 3 21 21", "M10.5 5.1c.5-.1 1-.1 1.5-.1 6.5 0 10 7 10 7a19 19 0 0 1-3 3.7", "M6.3 6.3A19 19 0 0 0 2 12s3.5 7 10 7c1.6 0 3.1-.4 4.3-1.1", "M10 10a3 3 0 0 0 4 4"],
    search: ["m16 16 5 5"],
    clock: ["M12 7v5l3 2"],
    folder: ["M3 5h7l2 3h9v11H3z"],
    download: ["M12 3v12", "m7 10 5 5 5-5", "M4 15v6h16v-6"],
    external: ["M14 3h7v7", "m10 14 11-11", "M10 3H3v18h18v-7"],
  };
</script>

<svg class={className} width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width={name === "logo" ? brandStroke : stroke} stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">
  {#each paths[name] ?? paths.info as path}<path d={path} />{/each}
  {#if name === "sun"}<circle cx="12" cy="12" r="4" />{/if}
  {#if name === "search"}<circle cx="10" cy="10" r="7" />{/if}
  {#if name === "clock" || name === "info" || !paths[name]}<circle cx="12" cy="12" r="9" />{/if}
  {#if name === "eye"}<circle cx="12" cy="12" r="3" />{/if}
</svg>

<style>
  svg { display: inline-block; flex-shrink: 0; vertical-align: middle; }
</style>
