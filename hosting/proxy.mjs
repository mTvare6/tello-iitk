const upstream = "https://hello.iitk.ac.in";
const lifetime = 2 * 60 * 60;
const encoder = new TextEncoder();
const decoder = new TextDecoder();
let cachedKey;
let cachedSecret;

export const securityHeaders = {
    "X-Content-Type-Options": "nosniff",
    "Referrer-Policy": "same-origin",
    "X-Frame-Options": "DENY",
    "Content-Security-Policy": "default-src 'self'; script-src 'self' 'wasm-unsafe-eval' https://gc.zgo.at; style-src 'self'; connect-src 'self' blob: https://epestr.goatcounter.com/count; img-src 'self' blob: data: https://epestr.goatcounter.com/count; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
};

function local(url) {
    return ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
}

function cookieName(url) {
    return local(url) ? "hello_session" : "__Host-hello_session";
}

function encode(bytes) {
    return btoa(String.fromCharCode(...bytes)).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function decode(text) {
    return Uint8Array.from(atob(text.replaceAll("-", "+").replaceAll("_", "/")), c => c.charCodeAt(0));
}

async function key(env) {
    if (!env.SESSION_KEY) throw new Error("Session encryption is not configured");
    if (cachedSecret !== env.SESSION_KEY) {
        const bytes = decode(env.SESSION_KEY);
        if (bytes.length !== 32) throw new Error("Invalid session encryption key");
        cachedKey = await crypto.subtle.importKey("raw", bytes, "AES-GCM", false, ["encrypt", "decrypt"]);
        cachedSecret = env.SESSION_KEY;
    }
    return cachedKey;
}

async function unseal(request, env) {
    const url = new URL(request.url);
    const value = (request.headers.get("Cookie") || "").split(";").map(v => v.trim()).find(v => v.startsWith(cookieName(url) + "="))?.split("=").slice(1).join("=");
    if (value && value.length <= 3800) {
        try {
            const bytes = decode(value);
            const plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv: bytes.subarray(0, 12), additionalData: encoder.encode(url.origin) }, await key(env), bytes.subarray(12));
            const jar = JSON.parse(decoder.decode(plain));
            if (jar.expires > Date.now() && Array.isArray(jar.cookies) && jar.cookies.length <= 40 && (!jar.challenge || (Array.isArray(jar.challenge) && jar.challenge.length <= 40))) return jar;
        } catch (_) {}
    }
    return { expires: Date.now() + lifetime * 1000, cookies: [] };
}

async function seal(jar, env, url) {
    jar.expires = Date.now() + lifetime * 1000;
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const encrypted = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv, additionalData: encoder.encode(url.origin) }, await key(env), encoder.encode(JSON.stringify(jar))));
    const bytes = new Uint8Array(iv.length + encrypted.length);
    bytes.set(iv); bytes.set(encrypted, iv.length);
    const value = encode(bytes);
    if (value.length > 3800) throw new Error("Upstream session is too large");
    return `${cookieName(url)}=${value}; HttpOnly; SameSite=Strict; Path=/; Max-Age=${lifetime}${url.protocol === "https:" ? "; Secure" : ""}`;
}

function account(jar) {
    const cookies = jar.cookies.filter(c => !c.expires || c.expires > Date.now());
    const uid = cookies.find(c => c.name === "uid")?.value;
    if (!uid || uid === "0" || !/^\d{1,32}$/.test(uid) || !cookies.some(c => /^S?SESS/.test(c.name))) return null;
    let username = cookies.find(c => c.name === "username")?.value || "";
    try { username = decodeURIComponent(username); } catch (_) {}
    return { uid, username };
}

function updateCookies(headers, jar) {
    for (const text of headers.getSetCookie()) {
        const parts = text.split(";");
        const first = parts.shift();
        const equals = first.indexOf("=");
        if (equals < 1) continue;
        const name = first.slice(0, equals).trim(), value = first.slice(equals + 1).trim();
        if (!/^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/.test(name) || /[\r\n;]/.test(value)) continue;
        const cookie = { name, value, path: "/", expires: 0 };
        let allowed = true;
        for (const part of parts) {
            const at = part.indexOf("=");
            const k = (at < 0 ? part : part.slice(0, at)).trim().toLowerCase();
            const v = at < 0 ? "" : part.slice(at + 1).trim();
            if (k === "domain" && !["hello.iitk.ac.in", ".hello.iitk.ac.in"].includes(v.toLowerCase())) allowed = false;
            if (k === "path" && v.startsWith("/")) cookie.path = v;
            if (k === "expires") cookie.expires = Date.parse(v) || 0;
            if (k === "max-age" && /^-?\d+$/.test(v)) cookie.expires = Date.now() + Number(v) * 1000;
        }
        if (!allowed) continue;
        jar.cookies = jar.cookies.filter(c => !(c.name === cookie.name && c.path === cookie.path));
        if (!cookie.expires || cookie.expires > Date.now()) jar.cookies.push(cookie);
    }
    jar.cookies = jar.cookies.filter(c => !c.expires || c.expires > Date.now());
    if (jar.cookies.length > 40) throw new Error("Upstream session is too large");
}

export function allowedPath(path, method) {
    const url = new URL(path, upstream);
    if (url.origin !== upstream || url.username || url.password || !path.startsWith("/")) return false;
    if (method === "POST") return url.pathname === "/user/login";
    if (method !== "GET") return false;
    if (url.pathname === "/user/login" || /^\/courses(?:\/[A-Za-z0-9_-]+)*\/?$/.test(url.pathname)) return true;
    if (/^\/image-captcha-generate\/\d+\/\d+\/?$/.test(url.pathname)) return true;
    if (/^\/studentapi\/[A-Za-z0-9_-]+\/(?:user_permissions\/allowed|first_course_handouts|lectures|announcements\/type\/all|resources|assignments|quizzes|exams)\/?$/.test(url.pathname)) return true;
    if (/\/(?:submit|delete|update|progress|complete|messages?)(?:\/|$)/i.test(url.pathname)) return false;
    return /^\/manage\/[A-Za-z0-9_-]+\/.+/.test(url.pathname) || url.pathname.startsWith("/sites/default/files/");
}

async function forward(path, method, body, jar, signal) {
    let url = new URL(path, upstream);
    const signingIn = method === "POST";
    for (let hop = 0; hop < 8; hop++) {
        const loginLanding = signingIn && method === "GET" && /^\/(?:user\/\d+\/?)?$/.test(url.pathname);
        if (!allowedPath(url.pathname + url.search, method) && !loginLanding) throw new Error("Unexpected service route");
        const headers = new Headers({ "User-Agent": "hello-iitk/0.1 (read-only course client)", "Accept": "*/*" });
        const cookies = jar.cookies.filter(c => url.pathname.startsWith(c.path) && (!c.expires || c.expires > Date.now()));
        if (cookies.length) headers.set("Cookie", cookies.map(c => `${c.name}=${c.value}`).join("; "));
        if (method === "POST") {
            headers.set("Content-Type", "application/x-www-form-urlencoded");
            headers.set("Origin", upstream);
            headers.set("Referer", upstream + "/user/login");
        }
        const response = await fetch(url, { method, headers, body: method === "POST" ? body : undefined, redirect: "manual", cache: "no-store", signal });
        updateCookies(response.headers, jar);
        if (![301, 302, 303, 307, 308].includes(response.status)) return { response, url };
        const location = response.headers.get("Location");
        await response.body?.cancel();
        if (!location) throw new Error("Invalid service redirect");
        url = new URL(location, url);
        if (url.origin !== upstream) throw new Error("Unexpected service redirect");
        if ([301, 302, 303].includes(response.status)) { method = "GET"; body = undefined; }
    }
    throw new Error("Too many service redirects");
}

function secured(response, api = false) {
    const headers = new Headers(response.headers);
    for (const [name, value] of Object.entries(securityHeaders)) headers.set(name, value);
    if (api) headers.set("Cache-Control", "no-store");
    return new Response(response.body, { status: response.status, headers });
}

function failure(message, status = 400) {
    return secured(new Response(message, { status, headers: { "X-Hello-Error": "1", "Content-Type": "text/plain; charset=utf-8" } }), true);
}

export async function proxy(request, env) {
    const url = new URL(request.url);
    if (!env.SESSION_KEY) return failure("Session encryption is not configured", 503);
    if (request.headers.get("X-Hello-Client") !== "1") return failure("Client header required", 403);
    const origin = request.headers.get("Origin");
    if (origin && origin !== url.origin) return failure("Origin refused", 403);
    try {
        const jar = await unseal(request, env);
        if (url.pathname === "/api/bootstrap" && request.method === "GET") {
            return secured(Response.json({ ready: true, account: account(jar), transport: "wasm" }, { headers: { "X-Hello-Transport": "wasm" } }), true);
        }
        if (url.pathname !== "/api/request") return failure("Unknown API route", 404);
        const path = url.searchParams.get("path");
        if (!path || !allowedPath(path, request.method)) return failure("Service route refused", 403);
        let body;
        if (request.method === "POST") {
            if (!request.headers.get("Content-Type")?.startsWith("application/x-www-form-urlencoded")) return failure("Invalid login form", 400);
            const chunks = [];
            let size = 0;
            const reader = request.body?.getReader();
            if (reader) {
                while (true) {
                    const next = await reader.read();
                    if (next.done) break;
                    size += next.value.byteLength;
                    if (size > 16384) { await reader.cancel(); return failure("Login form is too large", 413); }
                    chunks.push(next.value);
                }
            }
            const bytes = new Uint8Array(size);
            let offset = 0;
            for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
            body = decoder.decode(bytes);
            const form = new URLSearchParams(body);
            if (form.get("form_id") !== "user_login_form") return failure("Unexpected login form", 400);
        }
        const isLogin = new URL(path, upstream).pathname === "/user/login";
        const isChallenge = isLogin || path.startsWith("/image-captcha-generate/");
        if (isLogin && request.method === "GET") jar.challenge = [];
        const session = isChallenge ? { cookies: jar.challenge || [] } : jar;
        const started = performance.now();
        const result = await forward(path, request.method, body, session, request.signal);
        const upstreamTime = performance.now() - started;
        if (isChallenge) jar.challenge = session.cookies;
        if (isLogin && request.method === "POST" && account(session)) {
            jar.cookies = session.cookies;
            delete jar.challenge;
        }
        const headers = new Headers();
        for (const name of ["content-type", "content-disposition"]) {
            const value = result.response.headers.get(name);
            if (value) headers.set(name, value);
        }
        headers.set("Server-Timing", `upstream;dur=${upstreamTime.toFixed(1)}`);
        headers.set("X-Hello-URL", result.url.href);
        headers.set("Set-Cookie", await seal(jar, env, url));
        const limit = /^(?:\/manage\/|\/sites\/default\/files\/)/.test(result.url.pathname) ? 512 * 1024 * 1024 : 32 * 1024 * 1024;
        let received = 0;
        const bounded = result.response.body?.pipeThrough(new TransformStream({ transform(chunk, controller) {
            received += chunk.byteLength;
            if (received > limit) { controller.error(new Error("Service response exceeds limit")); return; }
            controller.enqueue(chunk);
        } }));
        return secured(new Response(bounded, { status: result.response.status, headers }), true);
    } catch (_) {
        return failure("Could not complete the HelloIITK request. Retry or sign in again.", 502);
    }
}
