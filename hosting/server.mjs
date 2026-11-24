import { createServer } from "node:http";
import { Readable } from "node:stream";
import { proxy } from "./proxy.mjs";

const origins = new Set((process.env.PUBLIC_ORIGINS || "http://localhost:8787").split(","));
const env = { SESSION_KEY: process.env.SESSION_KEY };

if (!env.SESSION_KEY) throw new Error("SESSION_KEY is required");

const server = createServer(async (incoming, outgoing) => {
    const controller = new AbortController();
    incoming.on("aborted", () => controller.abort());
    outgoing.on("close", () => {
        if (!outgoing.writableFinished) controller.abort();
    });

    try {
        const host = incoming.headers.host;
        const origin = [...origins].find(value => new URL(value).host === host);

        if (!origin) {
            outgoing.writeHead(421).end();
            return;
        }

        const url = new URL(incoming.url, origin);

        if (url.origin !== origin || !url.pathname.startsWith("/api/")) {
            outgoing.writeHead(404).end();
            return;
        }

        const options = {
            method: incoming.method,
            headers: incoming.headers,
            signal: controller.signal,
        };

        if (!["GET", "HEAD"].includes(incoming.method)) {
            options.body = Readable.toWeb(incoming);
            options.duplex = "half";
        }

        const response = await proxy(new Request(url, options), env);

        for (const [name, value] of response.headers) {
            if (name !== "set-cookie") outgoing.setHeader(name, value);
        }

        const cookies = response.headers.getSetCookie();

        if (cookies.length) outgoing.setHeader("Set-Cookie", cookies);

        outgoing.writeHead(response.status);

        if (response.body) {
            Readable.fromWeb(response.body)
                .on("error", () => outgoing.destroy())
                .pipe(outgoing);
        } else outgoing.end();
    } catch (_) {
        if (outgoing.headersSent) outgoing.destroy();
        else outgoing.writeHead(502, { "Cache-Control": "no-store" }).end("Service request failed");
    }
});

server.headersTimeout = 15_000;
server.requestTimeout = 30_000;
server.maxConnections = 256;
server.listen(Number(process.env.PORT || 8787), "127.0.0.1");

for (const signal of ["SIGINT", "SIGTERM"]) {
    process.on(signal, () => {
        server.close(() => process.exit(0));
        setTimeout(() => process.exit(0), 5000).unref();
    });
}
