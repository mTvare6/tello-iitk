import init, { Client } from "__hello_wasm";
import { Zip, ZipPassThrough } from "fflate";

let initialized;

async function archive(files, name, signal) {
    if (files.length > 2000) throw new Error("Too many files for one archive; select a smaller branch");
    if (files.length === 1) return { type: "download", url: files[0].url, name: files[0].name };
    const parts = [];
    let failure;
    const done = new Promise((resolve, reject) => {
        const zip = new Zip((error, bytes, final) => {
            if (error) { failure = error; reject(error); return; }
            parts.push(bytes);
            if (final) resolve();
        });
        (async () => {
            let total = 0;
            for (const file of files) {
                const entry = new ZipPassThrough(file.name);
                zip.add(entry);
                signal.throwIfAborted();
                const response = await fetch(file.url, { signal });
                const reader = response.body.getReader();
                while (true) {
                    const { value, done } = await reader.read();
                    if (done) break;
                    total += value.byteLength;
                    if (total > 1024 * 1024 * 1024) throw new Error("Download batch exceeds 1 GiB");
                    signal.throwIfAborted();
                    entry.push(value, false);
                    if (total % (1024 * 1024) < value.byteLength) await new Promise(resolve => setTimeout(resolve, 0));
                    if (failure) throw failure;
                }
                entry.push(new Uint8Array(), true);
            }
            zip.end();
        })().catch(reject);
    });
    await done;
    const url = URL.createObjectURL(new Blob(parts, { type: "application/zip" }));
    setTimeout(() => URL.revokeObjectURL(url), 60000);
    return { type: "download", url, name };
}

export async function connect(bootstrap, receive, closed) {
    initialized ||= init({ module_or_path: new URL("./hello_iitk_bg.wasm", import.meta.url) });
    await initialized;
    const app = new Client(JSON.stringify(bootstrap));
    let active = true, pending = false, timer, lastFrame;
    const archives = new Set();
    const draw = () => {
        if (!active) return;
        pending = false;
        try {
            const frame = app.frame();
            if (frame !== lastFrame) { lastFrame = frame; receive({ type: "frame", frame: JSON.parse(frame) }); }
            for (const effect of JSON.parse(app.effects())) {
                if (effect.type === "files") {
                    if (archives.size >= 4) { app.message("Download packaging is busy; retry shortly."); schedule(); continue; }
                    const controller = new AbortController();
                    archives.add(controller);
                    archive(effect.files, effect.name, controller.signal).then(message => { if (active) receive(message); }).catch(error => { if (active) { app.message(error.message); schedule(); } }).finally(() => archives.delete(controller));
                } else receive(effect);
            }
            clearTimeout(timer);
            if (app.busy()) timer = setTimeout(schedule, 100);
        } catch (error) { connection.close(); closed(error.message); }
    };
    const schedule = () => { if (active && !pending) { pending = true; queueMicrotask(draw); } };
    const connection = {
        readyState: WebSocket.OPEN,
        send(text) {
            if (!active) return;
            if (app.input(text)) { connection.close(); closed("Terminal closed."); return; }
            schedule();
        },
        close() {
            if (!active) return;
            active = false;connection.readyState = WebSocket.CLOSED;
            clearTimeout(timer);window.removeEventListener("hello-update", schedule);
            for (const controller of archives) controller.abort();
            archives.clear();
            app.free();
        },
    };
    window.addEventListener("hello-update", schedule);
    return connection;
}
