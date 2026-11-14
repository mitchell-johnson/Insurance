// A local stand-in for the Azure services the processes call, so the whole
// system can run on one machine with nothing mocked inside it:
//
// - Microsoft Entra ID: the token endpoint workloads exchange their
//   identity at, and the signing keys services validate tokens against.
//   Tokens are real RS256 JWTs for the audience asked for, carrying the app
//   roles the caller has been assigned in `roles` below, so every service
//   checks its callers exactly as it does in production.
// - Blob Storage (write-once names, index tags), Key Vault (wrap, unwrap,
//   sign), Event Hubs and Communication Services (email and SMS accepted
//   and kept, never sent).
//
// The protocols follow `crates/azure/src/testing.rs`, which the Rust tests
// use in process. This one is a process of its own because the services
// under test here are processes too.
"use strict";
const http = require("http");
const crypto = require("crypto");

const KEY_VERSION = "v7";

function start({ port = 0, roles = {} } = {}) {
  const { publicKey, privateKey } = crypto.generateKeyPairSync("rsa", { modulusLength: 2048 });
  const jwk = publicKey.export({ format: "jwk" });
  const kid = "e2e-key-1";
  const blobs = new Map();
  const tags = new Map();
  const seen = [];
  const mail = [];
  let base = "";

  const b64url = (v) => Buffer.from(v).toString("base64url");
  function jwt(claims) {
    const head = b64url(JSON.stringify({ alg: "RS256", typ: "JWT", kid }));
    const body = b64url(JSON.stringify(claims));
    const signature = crypto.sign("RSA-SHA256", Buffer.from(`${head}.${body}`), privateKey).toString("base64url");
    return `${head}.${body}.${signature}`;
  }
  // A token for `client` to call `audience`, with the roles it was assigned there.
  function issue(tenant, client, audience) {
    const now = Math.floor(Date.now() / 1000);
    const held = (roles[client] || {})[audience] || [];
    return jwt({ iss: `${base}/${tenant}/v2.0`, aud: audience, sub: client, oid: client, azp: client, appid: client, tid: tenant, roles: held, iat: now, nbf: now - 60, exp: now + 3600, ver: "2.0" });
  }

  const send = (res, status, body, type) => {
    const data = body == null ? "" : Buffer.isBuffer(body) ? body : typeof body === "string" ? body : JSON.stringify(body);
    res.writeHead(status, { "content-type": type || (typeof body === "object" && !Buffer.isBuffer(body) ? "application/json" : "application/octet-stream"), "content-length": Buffer.byteLength(data) });
    res.end(data);
  };

  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      const body = Buffer.concat(chunks);
      const url = new URL(req.url, base);
      const parts = url.pathname.split("/").filter(Boolean).map(decodeURIComponent);
      const json = () => { try { return JSON.parse(body.toString() || "{}"); } catch { return {}; } };
      try {
        // Entra ID.
        if (req.method === "POST" && parts.length === 4 && parts.slice(1).join("/") === "oauth2/v2.0/token") {
          const form = new URLSearchParams(body.toString());
          const scope = form.get("scope") || "";
          const client = form.get("client_id") || "unknown";
          seen.push(`token ${client} ${scope}`);
          return send(res, 200, { access_token: issue(parts[0], client, scope.replace(/\/\.default$/, "")), expires_in: 3600, token_type: "Bearer" });
        }
        if (req.method === "GET" && parts.length === 4 && parts.slice(1).join("/") === "discovery/v2.0/keys") {
          return send(res, 200, { keys: [{ kty: "RSA", use: "sig", alg: "RS256", kid, n: jwk.n, e: jwk.e }] });
        }
        if (req.method === "GET" && url.pathname === "/__seen") return send(res, 200, { seen, mail, blobs: [...blobs.keys()] });
        // A scan result, as Defender for Storage writes it to a blob's index tags.
        if (req.method === "PUT" && url.pathname === "/__tags") {
          const { key, set } = json();
          tags.set(key, set || {});
          return send(res, 200, { ok: true });
        }
        if (!/^Bearer \S+/.test(req.headers.authorization || "")) return send(res, 403, { error: "no bearer token" });
        // Key Vault.
        if (req.method === "POST" && parts[0] === "keys") {
          const [, key, a, b] = parts;
          const input = json();
          if (a === "sign") return send(res, 200, { kid: `${base}/keys/${key}/${KEY_VERSION}`, value: `sig-over-${input.value}` });
          if (a === "wrapkey") return send(res, 200, { kid: `${base}/keys/${key}/${KEY_VERSION}`, value: `wrapped-${key}-${input.value}` });
          if (b === "unwrapkey") {
            const prefix = `wrapped-${key}-`;
            if (a !== KEY_VERSION) return send(res, 404, { error: "no such key version" });
            if (typeof input.value !== "string" || !input.value.startsWith(prefix)) return send(res, 400, { error: "not wrapped under this key" });
            return send(res, 200, { kid: `${base}/keys/${key}/${a}`, value: input.value.slice(prefix.length) });
          }
        }
        // Communication Services: accepted and kept.
        if (req.method === "POST" && (parts[0] === "emails:send" || parts[0] === "sms")) {
          mail.push({ kind: parts[0], body: json() });
          return send(res, 202, { id: crypto.randomUUID(), status: "Running" });
        }
        // Event Hubs.
        if (req.method === "POST" && parts.length === 4 && parts[1] === "partitions" && parts[3] === "messages") {
          seen.push(`send ${parts[0]}/${parts[2]}`);
          return send(res, 201, "");
        }
        if (req.method === "POST" && parts.length === 2 && parts[1] === "messages") {
          seen.push(`send ${parts[0]}`);
          return send(res, 201, "");
        }
        // Blob Storage.
        if (parts.length >= 2) {
          const key = parts.join("/");
          if (req.method === "PUT") {
            if (url.searchParams.get("comp") === "tags") { tags.set(key, {}); return send(res, 204, ""); }
            if (blobs.has(key) && req.headers["if-none-match"]) return send(res, 409, { error: "exists" });
            blobs.set(key, body);
            return send(res, 201, "");
          }
          if (req.method === "GET" || req.method === "HEAD") {
            if (url.searchParams.get("comp") === "list") {
              const prefix = url.searchParams.get("prefix") || "";
              const names = [...blobs.keys()].filter((k) => k.startsWith(parts[0] + "/" + prefix)).map((k) => k.slice(parts[0].length + 1));
              return send(res, 200, `<?xml version="1.0" encoding="utf-8"?><EnumerationResults><Blobs>${names.map((n) => `<Blob><Name>${n}</Name></Blob>`).join("")}</Blobs><NextMarker/></EnumerationResults>`, "application/xml");
            }
            if (!blobs.has(key)) return send(res, 404, { error: "no such blob" });
            if (url.searchParams.get("comp") === "tags") {
              const set = Object.entries(tags.get(key) || {}).map(([k, v]) => `<Tag><Key>${k}</Key><Value>${v}</Value></Tag>`).join("");
              return send(res, 200, `<?xml version="1.0" encoding="utf-8"?><Tags><TagSet>${set}</TagSet></Tags>`, "application/xml");
            }
            return send(res, 200, blobs.get(key));
          }
          if (req.method === "DELETE") return blobs.delete(key) ? send(res, 202, "") : send(res, 404, { error: "no such blob" });
        }
        // A container listing has one path part.
        if (req.method === "GET" && parts.length === 1 && url.searchParams.get("comp") === "list") {
          const prefix = url.searchParams.get("prefix") || "";
          const names = [...blobs.keys()].filter((k) => k.startsWith(parts[0] + "/" + prefix)).map((k) => k.slice(parts[0].length + 1));
          return send(res, 200, `<?xml version="1.0" encoding="utf-8"?><EnumerationResults><Blobs>${names.map((n) => `<Blob><Name>${n}</Name></Blob>`).join("")}</Blobs><NextMarker/></EnumerationResults>`, "application/xml");
        }
        seen.push(`unhandled ${req.method} ${req.url}`);
        send(res, 404, { error: "the stand-in has no such route", method: req.method, path: req.url });
      } catch (e) {
        send(res, 500, { error: String(e) });
      }
    });
  });

  return new Promise((resolve) => server.listen(port, "127.0.0.1", () => {
    base = `http://127.0.0.1:${server.address().port}`;
    resolve({ base, issue, seen, mail, blobs, tags, close: () => new Promise((done) => server.close(done)) });
  }));
}

module.exports = { start, KEY_VERSION };
