# Design system

Every screen this service ships is built from one stylesheet, one script and one typeface:
`crates/channels/ui/app.css`, `ui/app.js` (the `portal` object), `ui/theme.js` and `ui/inter.woff2`.
A page is an HTML file that declares which frame it sits in, lays out cards, and fills them
with the shared components. No page carries styles of its own.

This document is the contract. A new screen that follows it looks and behaves like the rest
without anyone designing it again.

## Principles

1. **One job on screen at a time.** A console is split into views (tabs). Each view holds the
   cards for one piece of work. Nobody scrolls past ten forms to find theirs.
2. **Say state in words.** A status is a pill with its words. Tone (green, amber, red) repeats
   what the words say, and each tone has its own dot shape, so colour is never the only sign.
3. **The answer sits beside the action.** The outcome of a button appears directly under the
   form that caused it, in a live region, so a screen reader announces it.
4. **Refusals are plain and whole.** A person who may not use a page sees one card that says so
   and what to do. They never see forms they cannot use.
5. **Nothing loads from elsewhere.** The content security policy allows this service's own
   files only. Icons are drawn by the script; the typeface is served from here.
6. **Every value is text.** Nothing is written with `innerHTML`. Service data cannot become markup.

## Tokens

All tokens are CSS custom properties on `:root` in `app.css`. Components use tokens only; a
literal colour in a component rule is a defect.

| Group | Tokens | Use |
|---|---|---|
| Ground | `--bg`, `--surface`, `--sunken`, `--hover` | Page, card, table heads and wells, row hover |
| Lines | `--line`, `--line-strong` | Card and table rules; field borders |
| Text | `--ink`, `--ink-2`, `--muted` | Body, labels, supporting text (all at least 4.5:1 on their ground) |
| Accent | `--accent`, `--accent-strong`, `--accent-soft`, `--on-accent`, `--focus` | The primary action, the selected tab, links, focus rings |
| Meaning | `--ok`, `--warn`, `--bad`, `--info`, each with `-bg` and `-line` | Pills and banners. Never decoration, never the accent |
| Rail | `--side`, `--side-2`, `--side-line`, `--side-ink`, `--side-muted`, `--side-accent` | The navigation rail, dark in both themes |
| Space | `--s1` 4, `--s2` 8, `--s3` 12, `--s4` 16, `--s5` 24, `--s6` 32, `--s7` 48 | All padding, gaps and margins |
| Type | `--t-xs` 12, `--t-s` 13, `--t-m` 15, `--t-l` 17, `--t-xl` 22, `--t-2xl` 28 | The whole scale; nothing sits between steps |
| Shape | `--r` 10, `--r-s` 6, `--shadow-1`, `--shadow-2` | Cards and dialogs; fields, buttons, tables |

Type is Inter (variable, SIL Open Font License, `ui/inter-LICENSE.txt`), with the system
sans-serif as fallback. Figures in tables are tabular so columns of money line up.

## Themes

There are two themes, light and dark, with the same tokens. The page follows the system
setting unless the person chooses with the theme button; the choice is kept in the browser
and applied by `theme.js` before the page is drawn, so the other theme never flashes.

- System setting: `@media (prefers-color-scheme: dark)` on `:root:not([data-theme="light"])`.
- A choice: `data-theme="dark"` or `data-theme="light"` on the root element.

Every token has a value in both. Adding a token means adding it in all three places in `app.css`.

## Templates

A page chooses its frame on `<body>`; `app.js` builds the frame around `<main>`.

| `data-shell` | For | Frame |
|---|---|---|
| `workforce` | Staff consoles | Navigation rail (grouped: Customers, Money, Platform), who is signed in, theme, sign out. Below 900px the rail becomes a drawer behind a menu button. |
| `partner` | Adviser and employer portals | One bar: the brand, who is signed in, theme, sign out. Partners are not shown the staff consoles. |
| `public` | Front page and service status | One bar: the brand and theme. |

`data-page` names the page (it marks the current item in the rail) and `data-realm` says who
signs in (`workforce` or `customers`), which gives the frame its sign-out button.

The skeleton of every page:

```html
<body data-shell="workforce" data-page="claims" data-realm="workforce">
<main>
  <header class="page"><div><h1>Claims console</h1><p>One line on what it is for.</p></div><p id="me" class="chip"></p></header>
  <div id="msg" aria-live="polite"></div>
  <section data-view="Queue"><h2>Work queue</h2> … </section>
  <section data-view="Claim"><h2>A claim</h2> … </section>
</main>
<script src="/ui/app.js"></script>
<script>/* the page's own script */</script>
```

- **Page head**: the name, one line of purpose, and a chip saying who the person is here (roles, seats).
- **Views**: any direct child of `<main>` with `data-view="Name"` joins that view. The frame builds
  the tab bar, shows one view at a time, keeps the view in the address (`#queue`) so a reload or a
  shared link returns to it, and supports arrow keys. `data-views="A,B,C"` on `<main>` sets the order.
  A tab whose every card is hidden from this person is not offered. A page hears changes with
  `portal.onView((key) => …)`, and moves to a view with `portal.view(key)`.
- **Cards**: a `<section>` with an `<h2>`. A later `<h2>` inside a card starts a ruled sub-part.
- **Gate**: sign-in on a laptop and the refusal card replace the whole page (`portal.refuse`).

The navigation and the front page's cards come from one list (`NAV` and `PARTNERS` in `app.js`).
A new console is added there once.

## Components

| Component | How | Notes |
|---|---|---|
| Button | `<button>` primary, `class="quiet"` secondary, `class="danger"` destructive | One primary per form. `portal.run` shows a spinner and ignores a second press while it works. |
| Field | `<div><label for>…</label><input></div>` inside `<div class="row">` | Label above the field. Rows wrap; at phone width each field takes the full width. |
| Table | `portal.table(headers, rows)` | Scrolls inside its own frame. A column of figures aligns right. A column headed Status, State, Standing, Result or Outcome renders as pills. Empty tables say so. |
| Pill | `portal.pill(words, tone?)` | Tone is read from the words when not given: ok, warn, bad, info or neutral. |
| Banner | `portal.say(el, kind, words)` or `class="banner warn"` | Kinds: info (default), ok, warn, bad. `bad` is an alert; the others are status. |
| Details | `portal.details(value)` | A record as labelled values; lists of records become a table. Money, dates and yes or no are worded. Raw JSON is never shown. |
| Figures | `portal.stats([[label, value, tone, note]])` | The few numbers a page leads with. |
| Dialog | `await portal.ask({ title, text, label, confirm, danger })` | For a reason or a confirmation before an action. Resolves to the answer or `null`. The browser's own prompt is never used. |
| Empty state | `portal.empty(words)` | A dashed well with a sentence, never a blank space. |
| Icon | `portal.icon(name)` | Drawn inline, 24 unit grid, 1.75 stroke, inherits colour. |
| Launcher | `portal.launcher(el)` | The front page's cards. |

## Accessibility

- Contrast: text tokens meet 4.5:1 on their grounds in both themes; pills and banners use the
  tone's own dark-on-tint pair.
- Keyboard: a skip link, visible focus rings (`--focus`), tabs with arrow keys, dialogs that trap
  focus and close on Escape (native `<dialog>`).
- Screen readers: outcomes land in `aria-live` regions; refusals are `role="alert"`; the rail is a
  labelled `<nav>` and marks the current page; icons are hidden from the accessibility tree.
- Motion: transitions are short and are removed under `prefers-reduced-motion`.
- Small screens: every page works at 390px wide. Only tables scroll sideways, inside their frame.

## Checks

`crates/channels/tests/browser/check.js` drives every page as each kind of person in a real
browser and fails on a script error, a content security policy violation, a browser dialog, or
missing content. After each journey it audits the page for a screen reader: every control has a label, every button and link a name, every table header cells, icons are hidden or named, IDs are unique, and the page has a language, a title, one main heading and a main region. `SHOTS=dir` saves a full-page screenshot of every page; `THEME=dark` and
`WIDTH=390` take them in the dark theme and at phone width. Look at the screenshots before
shipping a change to `app.css`.
