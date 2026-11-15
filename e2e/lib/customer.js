// A customer of the insurer as the customer page meets them: a record in the
// customer master (opened as new business opens one, before a policy is
// issued), a sign-in bound to that record, and the customer's phone, enrolled
// after identity verification, which holds the key that approves a browser.
//
// The phone app is not part of this stack. Its key is made here with Node's
// own crypto and its signatures are made the way the app makes them; the
// enrolment and every check of a signature are the system's.
"use strict";
const crypto = require("crypto");
const fs = require("fs");
const path = require("path");

/// A token from the Entra stand-in for a client of the stack, for one service.
async function token(stack, client, audience) {
  const r = await fetch(`${stack.azure}/${stack.tenant}/oauth2/v2.0/token`, { method: "POST", headers: { "content-type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ client_id: client, scope: `${audience}/.default`, grant_type: "client_credentials" }) });
  if (!r.ok) throw new Error(`no token for ${client}: ${r.status}`);
  return (await r.json()).access_token;
}

/// A call to the customer master as new business.
async function master(stack, method, pathname, body, key) {
  const headers = { authorization: `Bearer ${await token(stack, "ic-e2e-new-business", stack.audiences.customers)}`, "x-ic-caller": "new-business", "x-ic-purpose": "new_business" };
  if (body !== undefined) headers["content-type"] = "application/json";
  if (key) headers["idempotency-key"] = key;
  const r = await fetch(stack.customers + pathname, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await r.text();
  let data; try { data = JSON.parse(text); } catch { data = text; }
  return { status: r.status, ok: r.ok, data };
}

/// The phone's key pair: P-256, the public half as the uncompressed point the system takes, in base64.
function phoneKey() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("ec", { namedCurve: "P-256" });
  const raw = publicKey.export({ format: "jwk" });
  const point = Buffer.concat([Buffer.from([4]), Buffer.from(raw.x, "base64url"), Buffer.from(raw.y, "base64url")]);
  return { publicKey: point.toString("base64"), privateKey: privateKey.export({ format: "pem", type: "pkcs8" }) };
}
/// What the phone app does with a message: an ECDSA P-256 SHA-256 signature, r and s side by side, in base64.
const phoneSigns = (who, message) => crypto.sign("sha256", Buffer.from(message), { key: who.phone.privateKey, dsaEncoding: "ieee-p1363" }).toString("base64");

/// A new customer of this journey's own: their record in the customer master, their sign-in, and their phone.
/// Names and birth dates are random, because the master matches people on them.
async function customer(t, { phone = true } = {}) {
  const letters = crypto.randomBytes(8).toString("hex").replace(/\d/g, (d) => "ghijklmnop"[d]);
  const given = `C${letters.slice(0, 6)}`, family = `K${letters.slice(6)}`;
  const born = `${1960 + crypto.randomInt(40)}-0${1 + crypto.randomInt(9)}-1${crypto.randomInt(9)}`;
  const email = `${given.toLowerCase()}.${family.toLowerCase()}@example.nz`;
  const made = await master(t.stack, "POST", "/v1/parties", { kind: "person", facts: [
    { field: "given_name", value: given }, { field: "family_name", value: family }, { field: "date_of_birth", value: born }, { field: "email", value: email },
  ] }, t.ref("party"));
  if (!made.ok) throw new Error(`the customer master would not open a record: ${made.status} ${JSON.stringify(made.data)}`);
  const party = made.data.party;
  const who = { key: party, subject: `sub-${party}`, party, persona: "Customer", about: `${given} ${family}`, staff: false, given, family, born, email };
  await t.sys.internal("identity", { directory: { bind_subject: { subject: who.subject, party } } }, `bind-${party}`);
  if (phone) {
    who.phone = { device: `phone-${party}`, ...phoneKey() };
    await t.sys.internal("identity", { identity: { enrol_device: { device: who.phone.device, party, public_key: who.phone.publicKey, hardware_backed: true,
      enrolment: { via: "verification", reference: `idv-${party}` } } } }, `enrol-${party}`);
  }
  return who;
}

/// Set up the browser on the customer page's Signing view, approving its key from the customer's phone.
async function enrolBrowser(t, s, who) {
  await s.tab("Signing");
  await s.press("Set up this browser");
  await s.done("#sg-msg", "Approve it from your phone to finish");
  const device = (await s.$("#sg-device").innerText()).trim(), key = (await s.$("#sg-key").innerText()).trim();
  await t.does(who, "POST", "/app/v1/devices", { device, public_key: key, hardware_backed: false, approver: who.phone.device, signature: phoneSigns(who, `enrol\n${device}\n${key}`) }, "the phone approves the browser");
  await s.press("Check again");
  await s.sees("This browser can sign for you.", "#sign-state");
  return device;
}

module.exports = { customer, enrolBrowser, phoneSigns, master, token };
