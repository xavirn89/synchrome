---
name: "Synchrome"
description: "A quiet native macOS workspace for shared browser profiles."
colors:
  "bg": "#f5f8f7"
  "surface": "#ffffff"
  "sidebar": "#eaf1ef"
  "fg": "#203a37"
  "muted": "#59716c"
  "accent": "#176b5d"
  "accent-soft": "#e0f0e9"
  "line": "#d8e3de"
  "danger": "#af343d"
  "danger-soft": "#fbebed"
  "warning": "#855413"
  "warning-soft": "#fff4df"
  "info": "#2d6193"
  "info-soft": "#eaf2fa"
  "bg-dark": "#141f20"
  "surface-dark": "#1b292b"
  "sidebar-dark": "#102321"
  "fg-dark": "#e5eeeb"
  "muted-dark": "#a2b8b0"
  "accent-dark": "#8bdbc0"
  "accent-soft-dark": "#243d35"
  "line-dark": "#354844"
  "danger-dark": "#ffabb3"
  "danger-soft-dark": "#432d34"
  "warning-dark": "#edc481"
  "warning-soft-dark": "#3e3423"
  "info-dark": "#a4cafa"
  "info-soft-dark": "#26394b"
  "primary-text-dark": "#12251e"
  "danger-text-dark": "#3a151c"
typography:
  headline:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif"
    fontSize: "30px"
    fontWeight: 650
    lineHeight: 1.18
    letterSpacing: "-0.035em"
  title:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif"
    fontSize: "18px"
    fontWeight: 650
    letterSpacing: "-0.02em"
  subheading:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif"
    fontSize: "14px"
    fontWeight: 650
  body:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.6
  label:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif"
    fontSize: "13px"
    fontWeight: 600
  supporting:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif"
    fontSize: "12px"
  status:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif"
    fontSize: "11px"
    fontWeight: 600
    lineHeight: 1.4
  technical:
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace"
    fontSize: "12px"
rounded:
  "tag": "5px"
  "compact-control": "6px"
  "count": "7px"
  "control": "8px"
  "setup-context": "9px"
  "notice": "10px"
  "connection": "11px"
  "group": "12px"
  "dialog": "16px"
spacing:
  "2": "2px"
  "4": "4px"
  "8": "8px"
  "12": "12px"
  "16": "16px"
  "20": "20px"
  "24": "24px"
  "28": "28px"
  "32": "32px"
  "36": "36px"
  "40": "40px"
  "48": "48px"
components:
  button-primary:
    backgroundColor: "{colors.accent}"
    textColor: "{colors.surface}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "9px 15px"
  button-primary-dark:
    backgroundColor: "{colors.accent-dark}"
    textColor: "{colors.primary-text-dark}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "9px 15px"
  button-secondary:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.fg}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "9px 15px"
  button-quiet:
    backgroundColor: "transparent"
    textColor: "{colors.muted}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "9px 15px"
  button-danger:
    backgroundColor: "{colors.danger}"
    textColor: "{colors.surface}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "9px 15px"
  button-danger-dark:
    backgroundColor: "{colors.danger-dark}"
    textColor: "{colors.danger-text-dark}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "9px 15px"
  field:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.fg}"
    rounded: "{rounded.control}"
    padding: "10px 12px"
    width: "100%"
  nav-item:
    backgroundColor: "transparent"
    textColor: "{colors.muted}"
    rounded: "{rounded.control}"
    padding: "12px 13px"
  nav-item-active:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.accent}"
    rounded: "{rounded.control}"
    padding: "12px 13px"
  tag:
    backgroundColor: "{colors.accent-soft}"
    textColor: "{colors.accent}"
    rounded: "{rounded.tag}"
    padding: "4px 7px"
  connection:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.fg}"
    rounded: "{rounded.connection}"
  notice-info:
    backgroundColor: "{colors.info-soft}"
    textColor: "{colors.info}"
    rounded: "{rounded.notice}"
    padding: "15px 18px"
  notice-warning:
    backgroundColor: "{colors.warning-soft}"
    textColor: "{colors.warning}"
    rounded: "{rounded.notice}"
    padding: "15px 18px"
  notice-danger:
    backgroundColor: "{colors.danger-soft}"
    textColor: "{colors.danger}"
    rounded: "{rounded.notice}"
    padding: "15px 18px"
  dialog:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.fg}"
    rounded: "{rounded.dialog}"
    width: "min(480px, calc(100vw - 48px))"
---
# Design System: Synchrome

## Overview

**Creative North Star: "The quiet Mac workspace"**

Synchrome is an **Operate** surface: a macOS workspace for managing shared browser profiles. The interface explains which browser is connected to which profile, what has actually synchronized, and the next useful action. Restraint, scanability and familiar desktop behavior take priority over decorative expression.

Use a calm green-tinted neutral palette, one teal accent, native system typography and a compact desktop rhythm. Brand character comes from the linked-arrow mark, careful alignment and clear language. The workspace is not a marketing page, metrics dashboard or topology canvas.

`PRODUCT.md` owns product facts. This document owns presentation rules. `apps/desktop/src/styles.css` owns the implemented global tokens and shared classes; component styles consume those tokens. The frontmatter records the current light values and explicit dark counterparts. `.impeccable/design.json` extends it with actual shadows, motion, breakpoints and component previews; its synthesized tonal strips are preview metadata, not additional application palette tokens.

**Key Characteristics:**

- A profile-first hierarchy: Synchrome profile → Mac → browser connection.
- Quiet green neutrals with teal reserved for primary, current and positive meaning.
- Native system typography, compact spacing and immediate everyday navigation.
- Thin borders and tonal layers, with strong elevation for protected dialogs and action feedback.
- Truthful local state, clear next actions and expandable technical diagnostics.

## Colors

Green-tinted neutrals keep the desktop workspace calm; semantic accents explain state alongside words and icons. Exact color primitives live in the frontmatter, where the original CSS names remain recognizable and `-dark` names identify the dark-theme counterparts.

### Primary

- **Deep teal / pale mint teal** (`accent`, `accent-dark`): primary actions, current selection, links, focus and positive status.
- **Soft teal surfaces** (`accent-soft`, `accent-soft-dark`): selected appearance options, gentle hover, positive marks and text selection.
- **Deep teal button ink** (`primary-text-dark`): readable text on the pale dark-theme primary button and brand mark. Dark primary buttons use this dedicated ink rather than surface-colored text.

### Secondary

These are semantic accents, not independent brand palettes:

- **Amber attention** (`warning` and its dark/soft counterparts): recoverable attention, conflicts, offline changes and access guidance.
- **Red failure** (`danger` and its dark/soft counterparts): failed operations, field validation and destructive controls. `danger-text-dark` supplies dark ink on the pale dark-theme destructive button.
- **Blue pending** (`info` and its dark/soft counterparts): checking, connection setup, pending first sync and other information.

### Neutral

- **Green mist / deep green workspace** (`bg`, `bg-dark`): workspace and secondary surfaces.
- **White / dark green surface** (`surface`, `surface-dark`): forms, dialogs and primary content.
- **Pale / deep green sidebar** (`sidebar`, `sidebar-dark`): the navigation shell.
- **Deep green / near-white ink** (`fg`, `fg-dark`): primary text.
- **Muted green ink** (`muted`, `muted-dark`): secondary text, placeholders and neutral remote relationships.
- **Quiet green line** (`line`, `line-dark`): separators, control borders and scrollbars.

Profile initial marks use the implemented hue sequence (164, 217, 33, 282, 8), not status semantics. Their light background/ink pair is `hsl(var(--profile-hue) 30% 93%)` / `hsl(var(--profile-hue) 45% 30%)`; dark uses `hsl(var(--profile-hue) 18% 23%)` / `hsl(var(--profile-hue) 35% 79%)`. Keep semantic state outside these identity marks.

**The Semantic Color Rule.** Pair every state color with text and, where helpful, an icon; color alone never explains a connection or recovery action.

### Appearance and contrast

Appearance is System, Light or Dark, stored locally under `synchrome.appearance`. System follows OS changes. The shell sets both root `data-theme` and native CSS `color-scheme`; components do not keep independent theme state. Tokens cover forms, dialogs, technical details, selection, focus, scrollbars and disabled controls. Account and sync data do not belong in this presentation preference.

Computed sRGB contrast for the implemented token pairs, before disabled opacity:

| Text / surface | Light | Dark |
| --- | --- | --- |
| Foreground / surface | 12.20:1 | 12.70:1 |
| Muted / surface | 5.25:1 | 7.16:1 |
| Muted / workspace | 4.91:1 | 8.04:1 |
| Accent / soft accent | 5.40:1 | 7.24:1 |
| Danger / soft danger | 5.38:1 | 7.05:1 |
| Warning / soft warning | 5.88:1 | 7.45:1 |
| Information / soft information | 5.73:1 | 7.01:1 |
| Primary button text / accent | 6.37:1 | 9.92:1 |

Recompute contrast when changing these tokens; retain at least 4.5:1 for body text.

## Typography

**Body and heading font:** the native `-apple-system` / BlinkMacSystemFont stack on macOS, followed by Segoe UI and sans-serif fallbacks. The system face is an explicit macOS product decision. **Technical font:** ui-monospace / SFMono-Regular / Menlo / monospace, reserved for technical data.

The type is compact, restrained and familiar. Workspace headings establish hierarchy without overwhelming profile names, state labels or the next action. Use tabular numerals where counts are compared.

### Hierarchy

- **Headline:** the main workspace heading (30px, weight 650, line-height 1.18, tracking −0.035em). It reduces to 27px at the 850px breakpoint and 25px at 650px.
- **Title:** section headings (18px, weight 650, tracking −0.02em). Selected-profile headings use 24px, reducing to 21px at 850px.
- **Subheading:** compact group headings (14px, weight 650).
- **Body:** standard interface text (14px), with paragraph line-height 1.6.
- **Label:** field labels and ordinary buttons (13px, weight 600).
- **Supporting:** generally 12–13px; compact metadata can use 10–11px. Shared status labels use 11px, weight 600 and line-height 1.4.
- **Technical:** code uses 12px; expandable diagnostics commonly use 11px and contained scrolling.

The authentication story is a localized entry-view treatment: its headline is 42px with line-height 1.13, reducing to 36px at 960px. The form heading is 27px with line-height 1.25 and tracking −0.03em. Native dialog titles use 20px, line-height 1.35 and tracking −0.025em. These do not replace the workspace hierarchy. Heading tracking stays between −0.02em and −0.035em.

**The Technical Detail Rule.** Use human names and plain-language recovery in the everyday hierarchy. Rust, SQL, network payloads, IDs, paths, tokens, journals, snapshots and service endpoints belong below expandable Technical details.

## Layout

Use a 4px spacing rhythm with 2px optical adjustments where the established components require them. Related controls use 8–12px gaps, groups use 16–24px, and major sections use 28–36px. Avoid large padding around sparse metrics or equally weighted feature-card grids.

The workspace fills the window with a 196px sidebar and a flexible content column. The top bar is 65px high; main content starts with 34px/32px/48px padding. Profiles uses list and detail columns: the list starts at 185px with a 24px gap and a quiet divider. Preserve selection on refresh. Overview rows open the matching profile. Wrap action groups and long user names, and contain long technical strings with wrapping and scrolling.

### Information architecture

| Destination | User question | Contents |
| --- | --- | --- |
| Overview | What is connected and what needs me? | Profile summaries, attention and local browser summary |
| Profiles | Where does this shared profile live? | Searchable profile list, selected profile, connections grouped by Mac |
| Devices | What can I connect here? | Known Macs, detected Chrome/Helium profiles, browser access and connection actions |
| Activity | What changed or needs a decision? | Conflicts, resolution controls and observed sync events |
| Settings | How does Synchrome behave? | Appearance, account, cloud diagnostics and advanced service state |

Account access in the sidebar opens Settings. The profile hierarchy is **Synchrome profile → Mac → browser connection**. Display user names and browser names. Remote connections are relationships, never evidence of current remote sync health.

### Desktop adaptation

The application targets desktop window sizes. At 1100px the Profiles list contracts to 154px, gaps to 18px and main-content horizontal padding to 24px; secondary connection trails disappear. At 850px the sidebar becomes 70px, labels collapse to focusable icons, and the top bar becomes 56px. Accessible names and tooltips remain available. At 650px the Profiles list becomes a horizontal scrolling row, its search field is hidden, and the detail becomes a full-width region with a top divider. Action groups, notices, browser rows and dialog footers wrap.

Authentication uses a split story/form view, with a form width bounded to 360px. At 960px its padding and story headline reduce; at 760px it becomes one column with the wordmark above the form and the long story hidden. Dialogs reduce their gutters and internal padding at 540px. Dialog width and height stay bounded with internal scrolling. Respect the native window minimums and check the smallest supported window as well as a large desktop window.

**The Profile First Rule.** Keep contextual actions close to the selected profile or browser connection; preserve the profile → Mac → browser hierarchy when a window narrows.

## Elevation & Depth

Borders and tonal layering define most surfaces. A low selection shadow appears in light theme and is absent in dark theme. Strong elevation is reserved for modal dialogs and action feedback; the workspace does not use decorative lifted cards.

### Shadow vocabulary

- **Selection:** `--shadow` is `0 1px 3px rgb(23 50 40 / 5%)` in light theme and `none` in dark theme; used by current navigation and selected profile rows.
- **Dialog:** `0 24px 72px rgb(0 0 0 / 24%)`, paired with the native backdrop `rgb(8 17 19 / 46%)`.
- **Toast:** `0 10px 35px rgb(0 0 0 / 12%)`, used by temporary completion feedback.

**The Quiet Surface Rule.** Separate content with shared surface tones and thin borders; reserve strong elevation for modal dialogs and action feedback.

### Motion

Navigation and routine data refreshes are immediate. Shared button color and border feedback uses 150ms ease-out. A dialog enters over 180ms using opacity and a 6px vertical shift with `cubic-bezier(.16,1,.3,1)`. Action toasts use the same short arrival. The boot loading line indicates pending work with a 1s ease-in-out cycle. Do not add page choreography, ornamental graph motion or repeated entrance animations to lists.

| Before | After | Why |
| --- | --- | --- |
| Completed actions had no shared feedback treatment | A brief 180ms toast confirms the completed operation | Users can connect the action with its result |
| Startup exposed service progress as ordinary technical text | A single loading line shows that the workspace is being prepared | Pending state is clear without requiring knowledge of services |
| Motion had no common reduced-motion policy | `prefers-reduced-motion` disables animation, transitions and smooth scrolling | Keep the same state information without moving the interface |

Reduced motion retains the same state information, disables movement and smooth scrolling, and leaves a stationary marker in the boot loading line.

Morphicons is restricted to the password-visibility and quick appearance toggles. Pointer activation uses the snappy spring preset; keyboard/synthetic activation changes immediately. Set its reduced-motion policy explicitly to respect the OS preference. Navigation, repeated refreshes, lists and the brand mark remain immediate and static.

## Shapes

Standard controls have gently curved corners (8px); notices use 10px; browser connection groups use 11px; primary content groups use 12px; dialogs use 16px. Compact tags use 5px, diagnostic controls 6px and count pills 7px. Related boundaries use a 1px shared line color, with a dashed boundary for an unconnected browser empty state. Identity marks use rounded squares; account avatars and setup step indicators use circles.

`Icon.svelte` renders named Lucide geometry from `src/lib/iconography.js` on a 24px grid with a consistent 1.8px stroke, round caps and round joins. Default icon size is 20px. Use these shared Lucide SVG icons rather than Unicode glyphs or emoji. The logo uses the canonical opposing-arc geometry in `src/lib/brand.js`; `Icon.svelte` imports it with its dedicated 2.05-unit stroke. The logo and macOS assets follow `docs/brand.md`.

## Components

Reuse `.button`, `.primary`, `.secondary`, `.quiet`, `.field`, `.notice`, `.status` and `.tech` from the global stylesheet. New component styles consume CSS variables and the established patterns.

### Buttons

Ordinary targets are about 40–44px high; compact secondary targets are about 32–36px. Shared buttons use 8px corners, 9px/15px padding, an 8px icon gap, 13px text and weight 600. Primary actions use explicit verbs and teal; secondary actions use the surface and a line border; quiet actions use muted text and a transparent surface; destructive actions use failure red. Compact shared buttons are 34px high with 7px/11px padding and 12px text. Text actions have a 32px minimum height; icon actions are 36px square and need an accessible name.

Fine-pointer hover gives secondary, quiet and icon actions a soft accent surface, and darkens primary actions with `brightness(.94)`. Keyboard focus uses a visible 2px accent outline with a 3px offset; authentication inputs and the dialog close action use a 2px offset. Shared disabled buttons reduce opacity to .45. Explain constraints in visible copy rather than relying on a disabled button’s tooltip.

### Tags and status labels

Tags use soft teal, teal text, 5px corners and compact 4px/7px padding. Counts use tabular numerals. Status labels are compact text with a 6px circular mark; colors convey meaning alongside the label. Profile identity hue must not carry sync meaning.

### Containers and connections

Use surface-colored, 1px-bordered groups for profile rows, browser connections, detected browsers and devices. Connections use 11px corners; profile overview groups use 12px. Local connection actions sit near their summary; advanced controls appear below a divider in expandable details. Remote connections show that they belong to another Mac and do not claim a live sync state. Empty states explain the next action.

### Inputs and authentication

Shared inputs use a surface background, line border, 8px corners, a 42px minimum height and 10px/12px padding. Authentication inputs are 44px high. Carets and focus use teal; invalid inputs use a red boundary. Labels are explicit and associated with their inputs.

`AuthView.svelte` owns the entry form, validation and sign-up mode. Keep Email and Password labels, the `form.login` integration selector and correct `username`, `current-password` or `new-password` autocomplete values. Validate email and require at least eight password characters for sign-up. Explain validation beside its field and focus the first invalid input on submission. Offer Show/Hide password, preserve typed input after failure and prevent duplicate submissions.

Disable sign-in for explicit cloud configuration or authentication-service failure, not an unknown probe state. Errors name the problem and recovery, with optional technical details. Creating an account can sign the user in or require email confirmation. In the latter case, show Check your inbox and a return to Sign in; do not present successful account creation as an error. Sign out is confirmed and explains that sync pauses on this Mac while local browser data and shared profiles remain.

### Navigation

Navigation stays immediate. Items are 44px high with 8px corners, muted text and original SVG icons; the current item uses teal text, a surface fill, stronger text and the theme’s low selection shadow. Use `aria-current`, explicit accessible names and the existing integration selectors. Account access opens Settings.

### Native dialogs and setup

`Dialog.svelte` wraps native `<dialog>`, labels it with generated IDs, contains focus, restores the invoking focus target and protects busy operations from dismissal. The close action has an accessible name. Backdrop dismissal requires a primary pointer press and release outside the dialog; dragging from inside must not close it. Prevent duplicate mutations while a protected operation is in flight.

`App.svelte` applies `use:dialogTrigger` to controls that open dialogs. Its capture-phase click listener focuses the invoking control with `preventScroll: true` before `Dialog.svelte` records `document.activeElement`, because WKWebView does not focus mouse-clicked buttons by default. Preserve this action on new dialog triggers.

**The Invoking Control Rule.** Return focus to the invoking control when it remains in the document, including a control that was unfocused before a mouse click opened the dialog; verify this in the macOS app.

Standard dialog width is bounded to 480px; wide dialogs to 720px. Both use a 48px total window gutter and a maximum height of the viewport minus 64px. Headers use 28px padding and content uses 24px/28px/28px; below 540px, the gutter becomes 24px total, maximum height subtracts 32px and interior padding becomes 22px.

First connection introduces the relationship between a browser, a Synchrome profile and other Macs before browser selection. Keep Explore workspace available. Access problems such as Full Disk Access have specific recovery guidance. Manual creation makes an unconnected shared profile. Import uses the existing create-and-map API, then guides the user through the native connector and companion. Show only relevant setup steps. A stored link is pending; companion confirmation and the first successful upload are distinct milestones. Completion copy requires evidence of first sync.

Everyday UI calls a mapping a **connection**. The current v1 engine allows one local browser connection to each Synchrome profile per Mac. Explain this limit where users select a connection; never promise Chrome and Helium can both use that same profile on one Mac. A connection can be changed to another browser through existing APIs.

Disconnecting stops that browser’s link and keeps its local browser profile, bookmarks and files. Deleting a Synchrome profile requires typing its exact displayed name. Explain cloud deletion and local-data retention separately. Disconnect cloud connections before the existing safe deletion service; refresh after both success and partial failure. Do not claim a failed sequence was rolled back or completed.

### Notices, state and feedback

Notices have 10px corners, 15px/18px padding and a 12px icon gap. Match semantic ink and soft surface to their text. Use `role="alert"` for actionable errors and `role="status"` for completion feedback. Show an initial loading surface during boot, disable only affected actions during requests and preserve the rest of the workspace. Success feedback describes the operation actually completed.

Use the shared helpers in `src/lib/product.js`: `connectionState`, `profileState`, `cloudState` and `attentionSummary`. A connection summary describes the strongest actionable local state. A healthy or unknown remote link cannot hide a local conflict, failure or incomplete first sync. Prioritize concrete actions rather than badge color: conflicts come first, then errors or compatibility review, blocked browser setup, offline recovery and remaining confirmation/setup steps. The Overview attention action opens Activity for conflicts and the affected profile for setup or recovery.

- **Danger:** an operation failed and needs recovery. Give the action and retain technical details below it.
- **Warning:** conflict, offline queued changes, required browser confirmation or a blocked setup step.
- **Information:** connection pending, companion setup, first sync pending/in progress or changes being applied.
- **Positive:** captured changes are up to date, based on available native state.
- **Neutral:** unconnected profile or a connection recorded on another Mac.

Cloud status distinguishes checking, not configured, unavailable, partially available and connected. Unknown or incomplete probes remain checking; confirmed connected requires reachability and successful authentication, database and storage probes. This cloud summary does not replace local connection state.

Technical panels retain extension settings, preference modes, conflict options, Realtime and compaction diagnostics. Technical strings wrap and scroll within bounded regions. Device retirement concerns compaction membership; its copy must not imply disconnection or stopping sync.

## Do's and Don'ts

### Do:

- **Do** reuse shared CSS variables, controls and the native system font; keep body text at 14px and the established spacing rhythm.
- **Do** retain System, Light and Dark across every surface, including fields, dialogs, focus, scrollbars and technical panels.
- **Do** pair semantic color with state text and a useful recovery action; prioritize local conflicts, failures and incomplete first sync truthfully.
- **Do** keep contextual actions beside the selected profile or browser connection and preserve the profile → Mac → browser hierarchy.
- **Do** preserve semantic main/navigation/section landmarks, input labels, `aria-current`, icon-button names, visible focus, the skip link and status/error announcements.
- **Do** exercise frontend flows in both themes with keyboard navigation, errors, empty states and narrow desktop windows.
- **Do** verify the rebuilt macOS app for boot, authentication fields, native dialogs, dismissal/focus restoration, appearance and window resizing after substantial UI changes. Include opening from a previously unfocused mouse-clicked control and restoring focus after dismissal.

### Don't:

- **Don't** introduce independent palettes, decorative metrics grids, topology canvases, glyph icons or ornamental page/list motion into the workspace.
- **Don't** expose IDs, raw service errors or technical payloads as the everyday headline; keep advanced diagnostics accessible below it.
- **Don't** claim a stored connection is a completed import, a remote relationship proves live health, or partial deletion progress was rolled back.
- **Don't** promise more than one local browser connection per Synchrome profile per Mac or synchronize passwords, cookies, sessions or arbitrary browser files.
- **Don't** depend on hover, color or a disabled button’s tooltip alone to explain a constraint or next action.
- **Don't** store credentials, account state or sync data with the local appearance preference.
- **Don't** treat browser preview as proof of WKWebView behavior, native browser writes or live cloud synchronization. Its explicit in-memory adapter validates frontend interaction only; use dedicated browser profiles and authorized test accounts for native synchronization checks.
