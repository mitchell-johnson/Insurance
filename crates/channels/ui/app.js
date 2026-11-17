// Shared by every page: sign-in, calls to the portal service, and small DOM helpers.
// Nothing is ever written with innerHTML: every value from the service is set as text.
"use strict";
const portal = (() => {
  let config = null;
  const store = window.sessionStorage;

  function h(tag, attrs, ...children) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (k === "on") for (const [ev, fn] of Object.entries(v)) el.addEventListener(ev, fn);
      else if (v === true) el.setAttribute(k, "");
      else if (v !== false && v != null) el.setAttribute(k, v);
    }
    for (const c of children.flat(Infinity)) if (c != null && c !== false) el.append(c.nodeType ? c : document.createTextNode(String(c)));
    return el;
  }
  function clear(el) { while (el.firstChild) el.removeChild(el.firstChild); return el; }
  // Replace an element's content. Unlike append, it leaves out anything null.
  function put(el, ...children) { clear(el).append(...children.flat(Infinity).filter((c) => c != null && c !== false)); return el; }
  function money(minor, currency) {
    const n = Number(minor) / 100;
    try { return new Intl.NumberFormat(undefined, { style: "currency", currency: currency || "NZD" }).format(n); } catch { return n.toFixed(2) + " " + (currency || ""); }
  }
  // An icon, drawn here so that no image is ever fetched.
  const ICONS = {
    user: "M12 12a4 4 0 100-8 4 4 0 000 8zM4 20a8 8 0 0116 0", shield: "M12 3l8 3v6c0 4.5-3.2 8-8 9-4.8-1-8-4.5-8-9V6zM9 12l2 2 4-4",
    clipboard: "M9 4h6v3H9zM7 5H5v16h14V5h-2M9 12h6M9 16h4", users: "M9 11a3.5 3.5 0 100-7 3.5 3.5 0 000 7zM2.5 20a6.5 6.5 0 0113 0M16 4.3a3.5 3.5 0 010 6.4M18 14.5a6.5 6.5 0 013.5 5.5",
    message: "M4 5h16v11H9l-5 4z", file: "M6 3h8l4 4v14H6zM14 3v4h4M9 12h6M9 16h6", card: "M3 6h18v12H3zM3 10h18M7 15h3", ledger: "M5 3h14v18H5zM9 8h6M9 12h6M9 16h3", chart: "M4 20V4M4 20h16M8 16l4-5 3 3 4-6",
    percent: "M19 5L5 19M7.5 9a2 2 0 100-4 2 2 0 000 4zM16.5 19a2 2 0 100-4 2 2 0 000 4z", swap: "M7 7h13l-3-3M17 17H4l3 3", layers: "M12 3l9 5-9 5-9-5zM3 13l9 5 9-5",
    server: "M4 4h16v6H4zM4 14h16v6H4zM8 7h.01M8 17h.01", pulse: "M3 12h4l3-7 4 14 3-7h4", menu: "M4 6h16M4 12h16M4 18h16",
    sun: "M12 16a4 4 0 100-8 4 4 0 000 8zM12 2v2M12 20v2M4 12H2M22 12h-2M5 5l1.5 1.5M17.5 17.5L19 19M5 19l1.5-1.5M17.5 6.5L19 5", moon: "M20 14.5A8 8 0 019.5 4a8 8 0 1010.5 10.5z",
    out: "M9 4H5v16h4M15 8l4 4-4 4M19 12H9", ok: "M20 6L9 17l-5-5", warn: "M12 4l9 16H3zM12 10v4M12 17h.01", bad: "M12 21a9 9 0 100-18 9 9 0 000 18zM9 9l6 6M15 9l-6 6",
    info: "M12 21a9 9 0 100-18 9 9 0 000 18zM12 11v5M12 8h.01", lock: "M6 11h12v9H6zM8 11V8a4 4 0 018 0v3", briefcase: "M3 8h18v12H3zM9 8V5h6v3M3 13h18",
    building: "M4 21V5l8-2v18M12 9h8v12M8 8h.01M8 12h.01M8 16h.01M16 13h.01M16 17h.01M3 21h18",
  };
  const SVG = "http://www.w3.org/2000/svg";
  function svg(tag, attrs, ...children) {
    const el = document.createElementNS(SVG, tag);
    for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
    el.append(...children);
    return el;
  }
  const icon = (name) => svg("svg", { class: "icon", viewBox: "0 0 24 24", "aria-hidden": "true" }, svg("path", { d: ICONS[name] || ICONS.info }));

  // A state in words, with a tone when the words carry one. Colour is never the only sign:
  // the words stay, and each tone has its own dot shape.
  function tone(value) {
    const s = String(value).toLowerCase();
    if (/\b(fail|declin|reject|refus|denied|error|blocked|overdue|breach|red\b|lapsed|cancel|suspend|avoid|abandon|stalled|unavailable|no\b|down\b|not )/.test(s)) return "bad";
    if (/\b(wait|pending|await|held|hold|review|prepared|requested|proposed|due\b|read.only|amber|partial|disputed|in progress|draft|unlinked|referred|needs|degraded|checking)/.test(s)) return "warn";
    if (/\b(ok|active|approved|paid|complete|completed|posted|in force|green|yes|accepted|settled|done|released|stands|healthy|signed|published|sent|cleared|granted|resolved|ready|agreed|allowed|in place|on cover|met|checked)\b/.test(s)) return "ok";
    return "";
  }
  const pill = (value, kind) => h("span", { class: "pill " + (kind === undefined ? tone(value) : kind) }, value);
  const STATE = /^(status|state|standing|result|outcome|allowed|health)$/i;
  const FIGURE = /^-?([A-Z]{0,3}[$€£])?\d[\d,]*(\.\d+)?%?$/;
  const empty = (words) => h("p", { class: "empty" }, words || "Nothing to show.");
  function table(headers, rows) {
    // A column of figures lines up on the right, heading and all.
    const figures = headers.map((_, i) => rows.length > 0 && rows.every((r) => r[i] == null || r[i] === "" || typeof r[i] === "number" || (typeof r[i] === "string" && FIGURE.test(r[i]))) && rows.some((r) => r[i] != null && r[i] !== ""));
    const cell = (c, i) => {
      if (c && c.nodeType) return h("td", {}, c);
      if (Array.isArray(c)) return h("td", {}, c);
      const words = c == null ? "" : String(c);
      if (words && words.length <= 28 && STATE.test(String(headers[i]))) return h("td", {}, pill(words));
      return h("td", { class: figures[i] ? "num" : null }, words);
    };
    return h("div", { class: "scroll" }, h("table", {},
      h("thead", {}, h("tr", {}, headers.map((t, i) => h("th", { scope: "col", class: figures[i] ? "num" : null }, t)))),
      h("tbody", {}, rows.length ? rows.map((r) => h("tr", {}, r.map(cell)))
        : h("tr", {}, h("td", { colspan: headers.length, class: "empty" }, "Nothing to show.")))));
  }
  // The few figures a page leads with: [label, value, tone, note].
  const stats = (items) => h("div", { class: "stats" }, items.map(([label, value, kind, note]) => h("div", { class: "stat " + (kind || "") }, h("span", {}, label), h("strong", {}, value), note ? h("small", {}, note) : null)));
  // A value from the service in words a person reads: keys as phrases, amounts
  // in minor units as money, yes and no for flags, nested records as lists.
  const phrase = (k) => String(k).replace(/_minor$/, "").replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());
  function text(v, key) {
    if (v == null) return "–";
    if (typeof v === "boolean") return v ? "yes" : "no";
    if (typeof v === "number" && /_minor$/.test(key || "")) return money(v);
    // A moment kept as seconds since 1970 reads as a date and time.
    if (typeof v === "number" && v > 1e9 && v < 1e11 && /^(at|submitted|granted|issued|expires|since|enrolled|.*_at)$/.test(key || "")) return when(v);
    if (Array.isArray(v)) return v.map((x) => text(x)).join(", ");
    if (typeof v === "object") return Object.entries(v).map(([k, x]) => `${phrase(k)}: ${text(x, k)}`).join("; ");
    // An enumeration value (in_force) reads as words; identifiers stay as they are.
    return /^[a-z]+(_[a-z]+)+$/.test(v) ? v.replace(/_/g, " ") : String(v);
  }
  // A change asked for on a policy, in words: what it does, never its code.
  const FREQUENCY = { weekly: "weekly", fortnightly: "fortnightly", monthly: "monthly", quarterly: "quarterly", half_yearly: "half-yearly", yearly: "yearly" };
  function change(kind, payload) {
    const p = payload || {};
    switch (kind) {
      case "change_frequency": return `Pay ${FREQUENCY[p.frequency] || String(p.frequency || "at a new frequency").replace(/_/g, " ")}${p.billing_day ? `, on day ${p.billing_day} of the month` : ""}`;
      case "decline_indexation": return p.declined === false ? "Take this year's indexation increase" : "Decline this year's indexation increase";
      case "cancel_policy": return "Cancel the policy";
      case "reduce_cover": return `Reduce ${String(p.cover || "the cover").replace(/_/g, " ")} to ${p.sum_insured_minor != null ? money(p.sum_insured_minor) : "a lower amount"}`;
      case "notify_claim": return "Tell us about a claim";
      default: return String(kind).replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());
    }
  }
  // Where a request stands, in words. `asker` is true for the person who asked for it.
  function requestState(r, asker) {
    const s = r.state || r;
    switch (s.status) {
      case "awaiting_approval": return s.from === "customer" ? (asker ? "Waiting for the customer to approve" : "Waiting for you to approve") : s.from === "second_staff" ? "Waiting for a second member of staff" : "Waiting for a second administrator";
      case "pending": return r.approved_by && asker ? "Approved by the customer; being carried out" : "Being carried out";
      case "applied": return r.approved_by && asker ? "Approved by the customer and done" : "Done";
      case "rejected": return `Not done: ${s.message || String(s.code || "refused").replace(/_/g, " ")}`;
      case "cancelled": return r.declined_by || r.declined ? (asker ? "Declined by the customer" : "You declined it") : (asker ? "Withdrawn by you" : "Withdrawn");
      default: return String(s.status || "").replace(/_/g, " ");
    }
  }
  // A moment kept as seconds since 1970 (or an RFC 3339 string), as New Zealand writes it: 17 Nov 2026, 4:26 pm.
  function when(v) { const d = typeof v === "number" ? new Date(v * 1000) : new Date(v); return isNaN(d) ? String(v) : d.toLocaleString("en-NZ", { timeZone: "Pacific/Auckland", day: "numeric", month: "short", year: "numeric", hour: "numeric", minute: "2-digit" }); }
  // The day of such a moment: 17 Nov 2026.
  function day(v) { const d = typeof v === "number" ? new Date(v * 1000) : new Date(v); return isNaN(d) ? String(v) : d.toLocaleDateString("en-NZ", { timeZone: "Pacific/Auckland", day: "numeric", month: "short", year: "numeric" }); }
  function details(v, key) {
    if (v == null || typeof v !== "object") return h("span", {}, text(v, key));
    if (Array.isArray(v)) {
      if (!v.length) return h("span", { class: "muted" }, "None.");
      if (v.every((x) => x && typeof x === "object" && !Array.isArray(x))) {
        const keys = [...new Set(v.flatMap(Object.keys))];
        return table(keys.map(phrase), v.map((x) => keys.map((k) => text(x[k], k))));
      }
      return h("span", {}, text(v, key));
    }
    const entries = Object.entries(v);
    if (!entries.length) return h("span", { class: "muted" }, "None.");
    return h("dl", { class: "details" }, entries.flatMap(([k, x]) => [h("dt", {}, phrase(k)), h("dd", {}, x && typeof x === "object" ? details(x, k) : STATE.test(k) && typeof x === "string" && x.length <= 28 ? pill(text(x, k)) : text(x, k))]));
  }
  // A page that could not load. Refused outright, it shows nothing else:
  // forms the person may not use are only noise.
  function refuse(e) {
    const words = String(e.message).replace(/^./, (c) => c.toUpperCase());
    if (e.status !== 403) return say(document.getElementById("msg"), "bad", words);
    const realm = document.body.dataset.realm;
    gate(h("section", { class: "gate refused" }, icon("lock"), h("h2", {}, "This page is not open to you"),
      banner("bad", words),
      h("p", { class: "muted" }, (realm === "workforce" ? "If you need it for your work, ask a staff administrator for the role." : "If you think this is wrong, contact us.") + " Nothing you did here was recorded against you."),
      h("div", { class: "actions" }, h("a", { href: "/" }, "All portals"), realm ? h("button", { class: "quiet", type: "button", on: { click: () => signOut(realm) } }, "Sign in as someone else") : null)));
  }
  // What stands in front of a page: sign-in on a laptop, or a refusal. The page behind it is not shown.
  function gate(card) {
    const main = document.querySelector("main");
    for (const old of main.querySelectorAll(".gate")) old.remove();
    document.body.classList.toggle("gated", !!card);
    if (card) main.append(card);
  }
  const banner = (kind, words) => h("div", { class: "banner " + kind, role: kind === "bad" ? "alert" : "status" }, icon(kind || "info"), h("span", {}, words));
  function say(el, kind, text) { put(el, banner(kind, text)); }
  // A question that must be answered before an action goes ahead. Resolves to the
  // answer (or true when nothing is asked for), or null when the person backs out.
  function ask({ title, text, label, confirm, danger, type, keep }) {
    return new Promise((resolve) => {
      const input = label ? h("input", { id: "ask-input", type: type || "text", required: true, maxlength: 500, autocomplete: "off" }) : null;
      let answer = null;
      const box = h("dialog", { "aria-labelledby": "ask-title" }, h("form", { on: { submit: (ev) => { ev.preventDefault(); answer = input ? input.value.trim() : true; box.close(); } } },
        h("h2", { id: "ask-title" }, title), text ? h("p", {}, text) : null, input ? [h("label", { for: "ask-input" }, label), input] : null,
        h("div", { class: "actions" }, h("button", { type: "button", class: "quiet", on: { click: () => box.close() } }, keep || "Cancel"), h("button", { class: danger ? "danger" : null }, confirm || "Continue"))));
      box.addEventListener("close", () => { box.remove(); resolve(answer); });
      document.body.append(box);
      box.showModal();
    });
  }

  async function loadConfig() {
    if (!config) config = await (await fetch("/ui/config.json")).json();
    return config;
  }
  const b64url = (bytes) => btoa(String.fromCharCode(...new Uint8Array(bytes))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

  // Sign in. Development: a subject typed by the person. Production: the
  // authorization code flow with PKCE against Microsoft Entra.
  async function signIn(realm) {
    const cfg = await loadConfig();
    if (cfg.mode === "dev") {
      const key = "dev:" + realm;
      if (!store.getItem(key)) {
        // A laptop has no identity provider: the person says who they are. Never offered in production.
        await new Promise((resolve) => gate(h("section", { class: "gate" }, h("h2", {}, "Sign in"),
          h("p", { class: "muted" }, "This is a development copy. Say who you are; in production you sign in with your organisation's account."),
          h("form", { id: "dev-sign-in", on: { submit: (ev) => { ev.preventDefault(); store.setItem(key, JSON.stringify({ subject: ev.target.elements.subject.value.trim(), roles: ev.target.elements.roles.value.trim() })); resolve(); } } },
            h("div", {}, h("label", { for: "dev-subject" }, "Subject"), h("input", { id: "dev-subject", name: "subject", required: true, autocomplete: "off" })),
            h("div", {}, h("label", { for: "dev-roles" }, "Roles (comma separated, optional)"), h("input", { id: "dev-roles", name: "roles", autocomplete: "off" })),
            h("button", {}, "Sign in")))));
        gate(null);
      }
      return entered(realm);
    }
    const idp = cfg[realm];
    if (store.getItem("token:" + realm)) return entered(realm);
    const params = new URLSearchParams(window.location.search);
    const redirect = window.location.origin + window.location.pathname;
    const base = `${idp.authority}/${idp.tenant}/oauth2/v2.0`;
    if (params.get("code") && params.get("state") === store.getItem("pkce:state")) {
      const body = new URLSearchParams({ client_id: idp.client_id, grant_type: "authorization_code", code: params.get("code"), redirect_uri: redirect, code_verifier: store.getItem("pkce:verifier"), scope: idp.scope });
      const res = await fetch(base + "/token", { method: "POST", body });
      store.removeItem("pkce:verifier"); store.removeItem("pkce:state");
      if (!res.ok) throw new Error("Sign-in failed.");
      store.setItem("token:" + realm, (await res.json()).access_token);
      window.history.replaceState({}, "", redirect + (store.getItem("pkce:hash") || ""));
      store.removeItem("pkce:hash"); sent = null;
      return entered(realm);
    }
    const verifier = b64url(crypto.getRandomValues(new Uint8Array(48)));
    const state = b64url(crypto.getRandomValues(new Uint8Array(16)));
    const challenge = b64url(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier)));
    store.setItem("pkce:verifier", verifier); store.setItem("pkce:state", state);
    // The identity provider sends the person back without the address's fragment: keep the view and what it was opened with.
    if (window.location.hash) store.setItem("pkce:hash", window.location.hash);
    window.location.assign(base + "/authorize?" + new URLSearchParams({ client_id: idp.client_id, response_type: "code", redirect_uri: redirect, scope: idp.scope, state, code_challenge: challenge, code_challenge_method: "S256" }));
    await new Promise(() => {});
  }
  function signOut(realm) { store.removeItem("token:" + realm); store.removeItem("dev:" + realm); window.location.reload(); }

  async function signedIn(realm) {
    const cfg = await loadConfig();
    const headers = {};
    if (cfg.mode === "dev") {
      const dev = JSON.parse(store.getItem("dev:" + realm) || "{}");
      headers["x-dev-subject"] = dev.subject || "";
      if (dev.roles) headers["x-dev-roles"] = dev.roles;
    } else headers["authorization"] = "Bearer " + store.getItem("token:" + realm);
    return { cfg, headers };
  }
  // A page that holds a device key proves it on each call: given the method,
  // the path and the exact body sent, it returns the headers to add.
  let prover = null;
  // W3C trace context: each call begins a trace here in the browser, which
  // the portal and the services behind it continue (crates/telemetry). The
  // trace ID is the reference a failure carries (e.reference), the one
  // support looks up in Application Insights.
  function traceparent() {
    const hex = (n) => Array.from(crypto.getRandomValues(new Uint8Array(n)), (b) => b.toString(16).padStart(2, "0")).join("");
    return "00-" + hex(16) + "-" + hex(8) + "-01";
  }
  async function api(realm, method, path, body) {
    const { cfg, headers } = await signedIn(realm);
    headers.traceparent = traceparent();
    if (method !== "GET") headers["idempotency-key"] = crypto.randomUUID();
    // A file goes as it is; anything else as JSON.
    const raw = typeof Blob !== "undefined" && body instanceof Blob;
    if (body !== undefined) headers["content-type"] = raw ? body.type || "application/octet-stream" : "application/json";
    const sent = body === undefined ? undefined : raw ? body : JSON.stringify(body);
    if (prover && !raw) Object.assign(headers, await prover(method, path.split("?")[0], sent || ""));
    const res = await fetch(path, { method, headers, body: sent });
    const data = await res.json().catch(() => null);
    if (res.status === 401 && cfg.mode !== "dev") { store.removeItem("token:" + realm); throw new Error("Your sign-in has expired. Reload the page to sign in again."); }
    if (!res.ok) {
      // A reference nobody holds is most often a slip of the fingers: say so, and what to do.
      // The portal's own refusals carry `detail`; another service's body passed on carries `error.message`.
      const said = data && (data.detail || (data.error && data.error.message) || data.message);
      const vague = res.status === 404 && (!said || /^no such (resource|party)$/i.test(said));
      // A role named as the code spells it (PayeeMaintainer) reads as words.
      // A reference that carries such a word (required:...:PolicySchedule) is left as it is, to be copied.
      const detail = said && String(said).replace(/(?<![\w:/-])([A-Z][a-z]+)((?:[A-Z][a-z]+)+)\b/g, (w) => w.replace(/([a-z])([A-Z])/g, "$1 $2").toLowerCase());
      const e = new Error(vague ? "Nothing was found with that reference. Check it for a slip and try again; if it is right, it may not be open to you." : detail || "The request failed (" + res.status + ")."); e.status = res.status; e.code = data && (data.title || (data.error && data.error.code)); e.reference = headers.traceparent.split("-")[1]; throw e;
    }
    return data;
  }
  // Fetch a file as the signed-in person and hand it to the browser to save.
  async function download(realm, path, name) {
    const { headers } = await signedIn(realm);
    headers.traceparent = traceparent();
    if (prover) Object.assign(headers, await prover("GET", path.split("?")[0], ""));
    const res = await fetch(path, { headers });
    if (res.status === 404) throw new Error("That document is not available to you.");
    if (!res.ok) { const data = await res.json().catch(() => null); throw new Error((data && (data.detail || (data.error && data.error.message))) || "The download failed (" + res.status + ")."); }
    const url = URL.createObjectURL(await res.blob());
    const a = h("a", { href: url, download: name });
    document.body.append(a); a.click(); a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 10000);
    return "Downloaded " + name + ".";
  }
  // The documents among a policy's rows, each with a button to download it.
  function documents(realm, rows, out) {
    const docs = rows.filter((r) => r.kind === "document");
    if (!docs.length) return h("p", { class: "muted" }, "No documents yet.");
    return table(["Document", "Issued", ""], docs.map((r) => [r.body.title || r.body.document, r.body.issued_on || "",
      h("button", { class: "quiet", on: { click: run(out, () => download(realm, "/app/v1/documents/" + encodeURIComponent(r.body.document) + "/pdf", r.body.document + ".pdf")) } }, "Download")]));
  }
  // Run an action from a button or form, showing its outcome in `out`.
  // While it runs its button shows that it is working and takes no second press.
  function run(out, fn) {
    return async (ev) => {
      if (ev && ev.preventDefault) ev.preventDefault();
      const target = ev && (ev.submitter || ev.currentTarget);
      const button = target && target.tagName === "BUTTON" ? target : null;
      if (button && button.getAttribute("aria-busy") === "true") return;
      if (button) button.setAttribute("aria-busy", "true");
      try { const done = await fn(); if (typeof done === "string") say(out, "ok", done); }
      catch (e) { say(out, "bad", e.message); }
      finally { if (button) button.removeAttribute("aria-busy"); }
    };
  }

  // ---- Picking instead of typing. Nobody should have to know a reference to type it.
  // A field becomes a combobox: as the person types (or on focus, for a short list), it offers
  // matches from `source`, an array of { value, label, hint } or an async function of the text.
  // Choosing one puts its value in the field and calls `onPick(item)`. The field still takes a
  // value typed in full, for the person who has the reference on a letter in front of them.
  let picks = 0;
  function picker(input, source, opts = {}) {
    const id = (input.id || "pick") + "-picks-" + (++picks);
    const list = h("ul", { class: "picks", role: "listbox", id, hidden: true });
    const status = h("span", { class: "visually-hidden", "aria-live": "polite" });
    // When there is nothing to choose, say so under the field, in the flow of the form, never over it.
    const note = h("small", { class: "pick-note", id: id + "-note", "aria-live": "polite", hidden: true });
    let items = [], at = -1, timer = null, asked = 0;
    input.setAttribute("role", "combobox"); input.setAttribute("aria-autocomplete", "list");
    input.setAttribute("aria-expanded", "false"); input.setAttribute("aria-controls", id); input.autocomplete = "off";
    const holder = input.parentElement; holder.classList.add("pick-holder"); holder.append(list, status, note);
    const tell = (words) => { note.textContent = words || ""; note.hidden = !words; };
    const close = () => { list.hidden = true; input.setAttribute("aria-expanded", "false"); input.removeAttribute("aria-activedescendant"); at = -1; };
    const mark = (n) => {
      at = n; [...list.children].forEach((li, i) => li.setAttribute("aria-selected", String(i === n)));
      if (n >= 0 && list.children[n]) { input.setAttribute("aria-activedescendant", list.children[n].id); list.children[n].scrollIntoView({ block: "nearest" }); }
    };
    const choose = (item) => {
      // A person is shown by name once chosen; the page reads their reference from what onPick kept.
      tell(""); input.value = opts.label ? item.label || item.value : item.value; input.dataset.label = item.label || item.value; close();
      input.dispatchEvent(new Event("change", { bubbles: true }));
      if (opts.onPick) opts.onPick(item);
    };
    function show(found, text) {
      // A value typed in full is chosen already: the list would only stand in front of what comes next.
      if (text && found.some((i) => String(i.value).toLowerCase() === text.toLowerCase())) { tell(""); return close(); }
      items = found.slice(0, opts.max || 12);
      if (!items.length) { close(); return tell(text ? opts.none ?? "Nothing matches that." : opts.empty || "Nothing to choose from yet."); }
      tell("");
      put(list, items.map((item, n) => h("li", { role: "option", id: id + "-" + n, "aria-selected": "false",
        on: { mousedown: (ev) => { ev.preventDefault(); choose(item); } } }, h("strong", {}, item.label || item.value), item.hint ? h("span", {}, item.hint) : null)));
      list.hidden = false; input.setAttribute("aria-expanded", "true"); at = -1;
      status.textContent = items.length ? `${items.length} to choose from.` : "";
    }
    async function lookup() {
      const text = input.value.trim(), mine = ++asked;
      if (typeof source !== "function") {
        const want = text.toLowerCase();
        return show(source.filter((i) => !want || `${i.value} ${i.label || ""} ${i.hint || ""}`.toLowerCase().includes(want)), text);
      }
      if (text.length < (opts.min == null ? 2 : opts.min)) { tell(""); return close(); }
      // A whole list fetched on focus (min 0) narrows as the person types; a search has already matched.
      const want = text.toLowerCase(), near = (i) => !want || `${i.value} ${i.label || ""} ${i.hint || ""}`.toLowerCase().includes(want);
      try { const found = (await source(text)) || []; if (mine === asked) show(opts.min === 0 ? found.filter(near) : found, text); }
      catch (e) { if (mine === asked) { close(); tell(e.message); } }
    }
    input.addEventListener("input", () => { delete input.dataset.label; clearTimeout(timer); timer = setTimeout(lookup, typeof source === "function" ? 250 : 0); });
    input.addEventListener("focus", () => { if (typeof source !== "function" || opts.min === 0) lookup(); });
    input.addEventListener("blur", () => setTimeout(close, 120));
    input.addEventListener("keydown", (ev) => {
      if (ev.key === "ArrowDown") { ev.preventDefault(); if (list.hidden) lookup(); else mark(Math.min(at + 1, items.length - 1)); }
      else if (ev.key === "ArrowUp") { ev.preventDefault(); mark(Math.max(at - 1, 0)); }
      else if (ev.key === "Enter" && !list.hidden && at >= 0 && items[at]) { ev.preventDefault(); choose(items[at]); }
      else if (ev.key === "Escape" && !list.hidden) { ev.preventDefault(); close(); }
    });
    return { set: (next) => { source = next; }, close };
  }
  // A field that takes several picks, kept as a comma-separated value.
  function pickMany(input, source, opts = {}) {
    const chosen = h("div", { class: "picked" }), all = () => String(input.dataset.many || "").split(",").filter(Boolean), names = {};
    const draw = () => put(chosen, all().map((v) => h("span", { class: "pill" }, names[v] || v, h("button", { type: "button", class: "quiet", "aria-label": "Remove " + (names[v] || v), on: { click: () => { input.dataset.many = all().filter((x) => x !== v).join(","); draw(); } } }, "×"))));
    input.parentElement.append(chosen);
    picker(input, source, { ...opts, onPick: (item) => { names[item.value] = item.label || item.value; input.dataset.many = [...new Set([...all(), item.value])].join(","); input.value = ""; draw(); } });
    const add = (value, label) => { names[value] = label || names[value] || value; input.dataset.many = [...new Set([...all(), value])].join(","); draw(); };
    return { values: () => [...all(), ...String(input.value).split(",").map((x) => x.trim()).filter(Boolean)], add, clear: () => { input.dataset.many = ""; input.value = ""; draw(); } };
  }

  // ---- Defaults a person should not have to work out.
  // Today and this month as the books reckon them: the business date the page was served with,
  // and New Zealand's calendar only while the core has reported none.
  const nzDate = (d) => new Intl.DateTimeFormat("en-CA", { timeZone: "Pacific/Auckland", year: "numeric", month: "2-digit", day: "2-digit" }).format(d || new Date());
  const booksDay = ((document.querySelector('meta[name="business-date"]') || {}).content || "").match(/^\d{4}-\d{2}-\d{2}$/) ? document.querySelector('meta[name="business-date"]').content : null;
  // A signed-in member of staff also hears it from /staff/v1/me.
  let books = null;
  const today = () => booksDay || books || nzDate();
  const month = (back = 0) => { const [y, m] = today().split("-").map(Number); const t = new Date(Date.UTC(y, m - 1 - back, 1)); return t.toISOString().slice(0, 7); };
  const days = (n, from) => { const d = new Date((from || today()) + "T00:00:00Z"); d.setUTCDate(d.getUTCDate() + n); return d.toISOString().slice(0, 10); };
  // A new reference for something this person is creating: readable, dated and unique enough
  // that two people starting the same kind of thing at once never collide.
  const reference = (prefix) => { const a = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789", r = crypto.getRandomValues(new Uint8Array(5)); return `${prefix}-${today().replace(/-/g, "")}-${Array.from(r, (b) => a[b % a.length]).join("")}`; };

  // ---- What the person is working on. The access rules ask why a record is opened; the answer is
  // the case, call or task in hand. It is kept for the sign-in's tab, so it is said once, not on every form.
  const WORK = "working-on";
  const working = () => store.getItem(WORK) || "";
  const workOn = (ref) => { if (ref) store.setItem(WORK, ref); else store.removeItem(WORK); for (const fn of workers) fn(working()); };
  const workers = [];

  // ---- Links between consoles. `go("claims", "claim", { claim: "CLM-1" })` opens that claim's view;
  // the page reads what it was sent with `params()` once it has signed the person in.
  const go = (page, key, params) => "/" + page + "#" + (key || "") + (params && Object.keys(params).length ? "?" + new URLSearchParams(params) : "");
  const link = (page, key, params, words) => h("a", { href: go(page, key, params) }, words);
  let sent = null;
  function params() {
    if (sent) return sent;
    const hash = window.location.hash.slice(1), q = hash.indexOf("?");
    sent = Object.fromEntries(new URLSearchParams(q < 0 ? "" : hash.slice(q + 1)));
    return sent;
  }

  // ---- Shared lists, asked once per page.
  const cache = {};
  const once = (key, fn) => (cache[key] = cache[key] || fn().catch((e) => { delete cache[key]; throw e; }));
  // Colleagues who may take `action` today (everyone current, with none), as picker items.
  async function colleagues(action) {
    const { people } = await once("colleagues:" + (action || ""), () => api("workforce", "GET", "/staff/v1/colleagues" + (action ? "?action=" + encodeURIComponent(action) : "")));
    return people.map((p) => ({ value: p.party, label: p.party, hint: [p.team, (p.roles || []).join(", ")].filter(Boolean).join(" · ") }));
  }
  // Customers and policies from what the person knows, under the case they are working on.
  async function find(text) {
    // Every search is recorded against the case in hand; without one there is no search, and the person is told why.
    if (!working()) throw new Error("Say which case, call or task you are working on first: every search is recorded against it.");
    const q = new URLSearchParams({ q: text, case: working() });
    return api("workforce", "GET", "/staff/v1/find?" + q);
  }
  const born = (d) => d ? new Date(d + "T00:00:00Z").toLocaleDateString("en-NZ", { day: "numeric", month: "short", year: "numeric", timeZone: "UTC" }) : "";
  // Picker sources over find: people (value is the party) and policies (value is the policy).
  async function people(text) {
    const r = await find(text);
    if (r.hint) throw new Error(r.hint.replace(/^./, (c) => c.toUpperCase()) + ".");
    if (r.customer_master_unavailable) throw new Error("The customer records cannot be reached just now. Try again in a minute.");
    return r.people.map((p) => p.closed ? { value: p.party, label: "Closed to you", hint: "Someone you are connected to, or yourself" }
      : { value: p.party, label: p.name || p.party, hint: [p.born ? "born " + born(p.born) : "", `${p.policies.length} polic${p.policies.length === 1 ? "y" : "ies"}`].filter(Boolean).join(" · "), person: p });
  }
  async function policies(text) {
    const r = await find(text), out = [], seen = new Set();
    const add = (p, who) => { if (p.closed || seen.has(p.policy)) return; seen.add(p.policy); out.push({ value: p.policy, label: p.number || p.policy, hint: [who, p.product, String(p.status || "").replace(/_/g, " ")].filter(Boolean).join(" · "), policy: p }); };
    for (const p of r.policies) add(p, "");
    for (const person of r.people) for (const p of person.policies || []) add(p, `${person.name || person.party}${p.as === "owner" ? "" : " (" + p.as + ")"}`);
    if (!out.length && r.hint) throw new Error(r.hint.replace(/^./, (c) => c.toUpperCase()) + ".");
    return out;
  }

  // ---- The frame around every page: navigation, who is signed in, theme, and the page's views.
  // [path, name, icon, what it is for]
  const PARTNERS = [["customer", "Your policies", "shield", "Your cover, bills and claims, changes waiting for you, and your details."], ["adviser", "Adviser portal", "briefcase", "Your book, your standing, and anything held from you."], ["employer", "Employer portal", "building", "Your scheme, its members and its administrators."]];
  const NAV = [
    ["Customers", [["staff", "Staff console", "user", "Policies, customer requests, your access and the held balance workbench."], ["records", "Customer records", "users", "Find a person, correct their record, relationships and consents, add someone, and settle duplicates."], ["claims", "Claims console", "shield", "Work queues, a claim, and acting on it as yourself."],
      ["underwriting", "Underwriting", "clipboard", "Queues, cases and decisions, disclosures, non-disclosure remedies and peer review."], ["group", "Group schemes", "users", "The exception queue, schemes, files, members, possible matches and file layouts."],
      ["complaints", "Complaints and conduct", "message", "The register, timers, referrals and conduct cases."], ["documents", "Documents", "file", "Documents waiting, requested letters, templates and approvals, notice defects and legal holds."]]],
    ["Money", [["payments", "Payments", "card", "Runs, payments waiting, payees, refunds and reconciliation."], ["collections", "Collections", "clipboard", "Policies in arrears, calls, arrangements, hardship and dunning sequences."], ["finance", "Finance console", "ledger", "The month-end close, checks, journals and sign-off."],
      ["pricing", "Unit pricing console", "chart", "Valuations, price checks, approval, publication and price errors."], ["commission", "Commission console", "percent", "Advisers, engagements, payees, rules, statements and cases."],
      ["replacements", "Replacement matches", "swap", "Cover rewritten soon after cover was given up, and disputed matches to decide."], ["reinsurance", "Reinsurance exceptions", "layers", "Cessions, notices and settlements that need a person."]]],
    ["Platform", [["ops", "Operations console", "server", "Regions and leaders."], ["status", "Service status", "pulse", "Whether everything is working, in plain words."]]],
  ];
  const mark = () => svg("svg", { class: "mark", viewBox: "0 0 30 30", "aria-hidden": "true" }, svg("rect", { class: "a", width: 30, height: 30, rx: 8 }), svg("path", { class: "b", d: "M15 6.5l7 2.6v5.2c0 4-2.8 7.3-7 8.7-4.2-1.4-7-4.7-7-8.7V9.1z" }), svg("circle", { class: "c", cx: 15, cy: 14.5, r: 2.6 }));
  const local = (fn) => { try { return fn(window.localStorage); } catch { return null; } };
  const dark = () => { const set = document.documentElement.dataset.theme; return set ? set === "dark" : window.matchMedia("(prefers-color-scheme: dark)").matches; };
  function themeButton() {
    const button = h("button", { type: "button", on: { click: () => { const to = dark() ? "light" : "dark"; document.documentElement.dataset.theme = to; local((s) => s.setItem("theme", to)); paint(); } } });
    const paint = () => { put(button, icon(dark() ? "sun" : "moon"), h("span", { class: "label" }, dark() ? "Light" : "Dark")); button.setAttribute("aria-label", dark() ? "Use the light theme" : "Use the dark theme"); };
    paint();
    return button;
  }
  const who = h("div", { class: "who", hidden: true });
  const here = () => document.body.dataset.page;
  // Shown once someone is signed in: their name where the token carries one, else their subject.
  function entered(realm) {
    let name = "";
    try {
      const dev = JSON.parse(store.getItem("dev:" + realm) || "null");
      if (dev) name = dev.subject;
      else { const claims = JSON.parse(atob(store.getItem("token:" + realm).split(".")[1].replace(/-/g, "+").replace(/_/g, "/"))); name = claims.name || claims.preferred_username || ""; }
    } catch { name = ""; }
    const show = (as) => { if (name) { put(who, h("span", { class: "avatar", "aria-hidden": "true" }, name.replace(/^sub-/, "").slice(0, 2)), h("div", {}, h("strong", {}, name), h("span", {}, as))); who.hidden = false; } };
    show(realm === "workforce" ? "Signed in" : here() === "customer" ? "Customer" : "Partner");
    if (window.location.hash.length > 1) view(window.location.hash.slice(1).split("?")[0]);
    if (realm === "workforce") return standing(show);
  }
  // A member of the workforce: on the staff directory or not, and the books' business date. Someone not on the
  // directory (an operations engineer) is offered the platform pages only.
  async function standing(show) {
    let me = null;
    try { me = await api("workforce", "GET", "/staff/v1/me"); } catch { me = null; }
    if (me && me.business_date) books = me.business_date;
    const staff = !!(me && me.staff);
    show(staff ? "Staff" : "Not on the staff directory");
    if (!staff) for (const a of document.querySelectorAll("aside.side nav [data-group]")) if (a.dataset.group !== "Platform") a.hidden = true;
  }

  // Views: a page's cards grouped under tabs, one group on screen at a time.
  // A card joins a view with data-view="Name"; a page hears changes with onView.
  const slug = (name) => name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
  const listeners = [];
  let tabs = null;
  const parts = () => [...document.querySelectorAll("main > [data-view]")];
  function view(key) {
    if (!tabs) return;
    const tab = [...tabs.children].find((t) => t.dataset.key === key && !t.hidden);
    if (!tab) return;
    const was = tabs.querySelector('[aria-selected="true"]');
    for (const t of tabs.children) { t.setAttribute("aria-selected", String(t === tab)); t.tabIndex = t === tab ? 0 : -1; }
    for (const p of parts()) p.classList.toggle("off", slug(p.dataset.view) !== key);
    window.history.replaceState(null, "", "#" + key);
    if (was !== tab) for (const fn of listeners) fn(key);
  }
  function views() {
    const all = parts();
    if (!all.length) return;
    const order = document.querySelector("main").dataset.views;
    const names = order ? order.split(",") : [...new Set(all.map((p) => p.dataset.view))];
    tabs = h("div", { class: "tabs", role: "tablist", "aria-label": "Sections of this page" }, names.map((n) => h("button", { type: "button", role: "tab", "data-key": slug(n), "aria-selected": "false", on: { click: () => view(slug(n)) } }, n)));
    tabs.addEventListener("keydown", (ev) => {
      if (ev.key !== "ArrowRight" && ev.key !== "ArrowLeft") return;
      const open = [...tabs.children].filter((t) => !t.hidden), at = open.indexOf(document.activeElement);
      const next = open[(at + (ev.key === "ArrowRight" ? 1 : open.length - 1)) % open.length];
      next.focus(); next.click();
    });
    all[0].before(tabs);
    // A tab whose every card is hidden from this person is not offered.
    const sync = () => { for (const t of tabs.children) { const none = all.filter((p) => slug(p.dataset.view) === t.dataset.key).every((p) => p.hidden); if (t.hidden !== none) t.hidden = none; } };
    new MutationObserver(sync).observe(document.querySelector("main"), { attributes: true, attributeFilter: ["hidden"], subtree: true });
    sync();
    const first = [...tabs.children].find((t) => !t.hidden) || tabs.firstChild;
    first.setAttribute("aria-selected", "true");
    for (const p of all) p.classList.toggle("off", slug(p.dataset.view) !== first.dataset.key);
  }
  function frame() {
    const body = document.body, kind = body.dataset.shell, main = document.querySelector("main");
    if (!kind || !main) return;
    const realm = body.dataset.realm, here = body.dataset.page;
    main.id = "content"; main.tabIndex = -1;
    const brand = (href) => h("a", { class: "brand", href }, mark(), h("span", {}, "Insurance Core"));
    const out = realm ? h("button", { type: "button", id: "out", on: { click: () => signOut(realm) } }, icon("out"), h("span", { class: "label" }, "Sign out")) : null;
    const app = h("div", { class: "app" + (kind === "workforce" ? "" : " top") });
    const skip = h("a", { class: "skip", href: "#content" }, "Skip to the page");
    if (kind === "workforce") {
      const link = (group) => ([path, name, pic]) => h("a", { href: "/" + path, "data-group": group, "aria-current": path === here ? "page" : null }, icon(pic), name);
      const side = h("aside", { class: "side", id: "side" }, brand("/"),
        h("nav", { "aria-label": "Consoles" }, NAV.map(([group, items]) => [h("h2", { "data-group": group }, group), items.map(link(group))])),
        h("div", { class: "side-foot" }, who, h("div", { class: "tools" }, themeButton(), out)));
      const menu = h("button", { type: "button", "aria-label": "Menu", "aria-controls": "side", on: { click: () => app.classList.toggle("open") } }, icon("menu"));
      const scrim = h("button", { type: "button", class: "scrim", "aria-label": "Close the menu", on: { click: () => app.classList.remove("open") } });
      app.append(side, scrim, h("div", { class: "work" }, h("div", { class: "bar" }, menu, brand("/")), main));
    } else {
      app.append(h("div", { class: "bar" }, brand(kind === "public" ? "/" : "/" + here), who, themeButton(), out), main);
    }
    body.prepend(skip, app);
    views();
  }
  // The front page's cards, from the same list as the navigation.
  function launcher(el) {
    const card = ([path, name, pic, about]) => h("a", { href: "/" + path }, h("span", { class: "tile" }, icon(pic)), h("span", {}, h("strong", {}, name), h("span", {}, about)));
    put(el, [["Partners", PARTNERS], ...NAV].map(([group, items]) => [h("h2", { class: "group" }, group), h("div", { class: "launch" }, items.map(card))]));
  }
  // A form for every action a module takes from a person: pick the action, fill its fields, send.
  // `spec` maps an action's name to { label, can, fields: [[key, label, kind, options]], build, makes }.
  // A dotted key nests; kinds are text, optional, date, optdate, money, optmoney, int, zint, bool, nobool,
  // select, list, json and reference (a fresh reference made with `options` as its prefix).
  // `opts.send(command)` sends what `build(values, name)` returns, or { type: name, ...values } by default.
  function actions(el, spec, opts = {}) {
    const usable = Object.entries(spec).filter(([, a]) => !a.can || a.can());
    if (!usable.length) { put(el, null); return { choose: () => {} }; }
    const pick = h("select", { id: (opts.id || "act") + "-which" }, usable.map(([name, a]) => h("option", { value: name }, a.label || name.replace(/_/g, " "))));
    const fields = h("div", { class: "row" }), out = h("div", { "aria-live": "polite" }), note = h("p", { class: "muted" });
    const draw = () => {
      const a = spec[pick.value];
      note.textContent = a.note || "";
      put(fields, a.fields.map(([key, label, kind, options], n) => {
        const id = `${opts.id || "act"}-f${n}`;
        const input = kind === "select" ? h("select", { id }, options.map((o) => h("option", { value: o }, String(o).replace(/_/g, " "))))
          : kind === "bool" || kind === "nobool" ? h("select", { id }, h("option", { value: "true", selected: kind === "bool" }, "Yes"), h("option", { value: "false", selected: kind === "nobool" }, "No"))
          : kind === "json" ? h("textarea", { id, rows: 4 })
          : h("input", { id, type: /date/.test(kind) ? "date" : /money|int/.test(kind) ? "number" : "text", step: /money/.test(kind) ? "0.01" : null,
            required: !["optional", "optdate", "optmoney", "list", "zint"].includes(kind) });
        if (kind === "reference") input.value = reference(options || "REF");
        if (opts.prefill && opts.prefill[key] != null) input.value = opts.prefill[key];
        return h("div", {}, h("label", { for: id }, label), input);
      }));
      // A picker wraps its field, so it is attached once the field is on the page.
      a.fields.forEach(([key], n) => {
        const input = document.getElementById(`${opts.id || "act"}-f${n}`);
        if (opts.pickers && opts.pickers[key] && input && input.tagName === "INPUT") picker(input, opts.pickers[key], { min: 0 });
      });
    };
    const form = h("form", { on: { submit: run(out, async (ev) => {
      if (ev && ev.preventDefault) ev.preventDefault();
      const name = pick.value, a = spec[name], values = {};
      a.fields.forEach(([key, , kind], n) => {
        const raw = document.getElementById(`${opts.id || "act"}-f${n}`).value;
        const value = kind === "bool" || kind === "nobool" ? raw === "true" : kind === "list" ? raw.split(",").map((x) => x.trim()).filter(Boolean)
          : kind === "int" || kind === "zint" ? Number(raw || 0) : kind === "money" ? Math.round(Number(raw) * 100)
          : kind === "optmoney" ? (raw.trim() ? Math.round(Number(raw) * 100) : undefined) : kind === "optional" || kind === "optdate" ? (raw.trim() || undefined)
          : kind === "json" ? JSON.parse(raw || "null") : raw.trim();
        if (value === undefined) return;
        const path = key.split("."); let at = values;
        for (const part of path.slice(0, -1)) at = at[part] = at[part] || {};
        at[path[path.length - 1]] = value;
      });
      const answer = await opts.send(a.build ? a.build(values, name) : { type: name, ...values });
      if (opts.after) await opts.after(name, answer);
      // The reference this form made is the one recorded: say it, before the form makes the next.
      const made = a.fields.map(([key, , kind]) => kind === "reference" ? values[key] : null).filter(Boolean);
      const said = a.done ? await a.done(values, answer) : null;
      draw();
      if (said) return said;
      return (answer && answer.prepared) ? "Prepared. Someone else signs it before it is sent." : `Done: ${a.label || name.replace(/_/g, " ")}.${made.length ? ` Its reference is ${made.join(", ")}.` : ""}`;
    }) } }, h("div", { class: "row" }, h("div", {}, h("label", { for: pick.id }, "Action"), pick)), note, fields, h("p", {}, h("button", {}, "Send")));
    pick.addEventListener("change", draw);
    put(el, [form, out]);
    draw();
    return { choose: (name, prefill = {}) => { pick.value = name; draw(); a_fill(prefill); form.scrollIntoView({ behavior: "smooth", block: "start" }); } };
    function a_fill(prefill) { spec[pick.value].fields.forEach(([key], n) => { if (prefill[key] != null) document.getElementById(`${opts.id || "act"}-f${n}`).value = prefill[key]; }); }
  }
  frame();
  return { h, clear, put, money, table, text, details, say, refuse, signIn, signOut, api, run, loadConfig, download, documents, icon, pill, stats, empty, ask, view, onView: (fn) => listeners.push(fn), launcher, prove: (fn) => { prover = fn; },
    picker, pickMany, actions, today, month, days, reference, working, workOn, onWork: (fn) => workers.push(fn), go, link, params, once, colleagues, find, people, policies, born, when, day, change, requestState };
})();
