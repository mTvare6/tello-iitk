"use strict";

const terminal = document.getElementById("terminal");
const screen = document.getElementById("screen");
const keys = document.getElementById("keys");
const input = document.getElementById("input");
const accessible = document.getElementById("accessible-screen");
const graphemes = new Intl.Segmenter(undefined, { granularity: "grapheme" });
const cellHeight = 20;
const measure = node("span", "M".repeat(100), "measure");
measure.setAttribute("aria-hidden", "true");
document.body.append(measure);
const cellWidth = measure.getBoundingClientRect().width / 100;
measure.remove();

let socket,
    frame,
    lastSize = "",
    hintSignature = "",
    composing = false,
    pendingTab = null,
    connecting = false,
    viewportTimer,
    pointerFrame;
const coarse = matchMedia("(pointer: coarse)").matches;
const usernameStorageKey = "hello-iitk.username";
let restoreUsername = true,
    lastUsername = null;

function rememberUsername(next) {
    if (typeof next.username !== "string") return;

    try {
        if (restoreUsername) {
            restoreUsername = false;
            const saved = localStorage.getItem(usernameStorageKey);

            if (
                !next.username &&
                saved &&
                saved.length <= 60 &&
                !/[\u0000-\u001f\u007f]/.test(saved)
            ) {
                send({ type: "restore_username", username: saved });

                return;
            }
        }

        if (next.username !== lastUsername) {
            if (next.username)
                localStorage.setItem(usernameStorageKey, next.username);
            else localStorage.removeItem(usernameStorageKey);

            lastUsername = next.username;
        }
    } catch (_) {}
}

function node(tag, text, className) {
    const n = document.createElement(tag);

    if (text !== undefined) n.textContent = text;

    if (className) n.className = className;

    return n;
}

function send(message) {
    if (socket?.readyState === WebSocket.OPEN)
        socket.send(JSON.stringify(message));
}

function sendText(text) {
    let bounded = "",
        count = 0;

    for (const character of text) {
        if (++count > 1024) break;

        bounded += character;
    }

    send({ type: "text", text: bounded });
}

function resize() {
    if (!socket || socket.readyState !== WebSocket.OPEN) return;

    const cols = Math.max(
        24,
        Math.min(240, Math.floor(terminal.clientWidth / cellWidth)),
    );
    const rows = Math.max(
        10,
        Math.min(100, Math.floor(terminal.clientHeight / cellHeight)),
    );
    const size = `${cols}x${rows}`;

    if (size !== lastSize) {
        lastSize = size;
        send({ type: "resize", cols, rows });
    }
}

function fitViewport() {
    document.body.style.height = `${window.visualViewport?.height || innerHeight}px`;
    clearTimeout(viewportTimer);
    viewportTimer = setTimeout(resize, 30);
}

function focusInput(force = false) {
    if (!frame?.input && !force) return;

    input.setAttribute("aria-label", frame.input || "Terminal input");
    input.enterKeyHint = frame.input === "username" ? "next" : "done";
    input.focus({ preventScroll: true });
    resetInput();
}

function resetInput() {
    input.value = "\u200b";
    input.setSelectionRange(1, 1);
}

function prepareOpen(key) {
    if (
        (frame?.opens_browser && ["Enter", "ArrowRight", "l", "o"].includes(key)) ||
        (frame?.mode === "login" &&
            key === "F4" &&
            frame.hints.some((hint) => hint.keys.includes("F4")))
    ) {
        if (!pendingTab || pendingTab.closed) {
            pendingTab = window.open("about:blank", "_blank");

            if (pendingTab) pendingTab.opener = null;
        }
    }
}

function keyboard(key, ctrl = false, alt = false, shift = false) {
    prepareOpen(key);
    send({ type: "key", key, ctrl, alt, shift });
}

function action(index, choice, key) {
    if (key === "TextInput") {
        focusInput();

        return;
    }

    prepareOpen(key);
    send({ type: "action", index, choice, key });

    if (frame?.input || ["/", "f"].includes(key)) focusInput(true);
}

const displayKeys = {
    ArrowUp: "↑",
    ArrowDown: "↓",
    ArrowLeft: "←",
    ArrowRight: "→",
    "Shift+Tab": "⇧Tab",
    Tab: "Tab",
};

function hints(next) {
    const signature = JSON.stringify(next);

    if (signature === hintSignature) return;

    hintSignature = signature;
    keys.replaceChildren();
    next.forEach((hint, index) => {
        const group = node("span", undefined, "hint");
        hint.keys.forEach((key, choice) => {
            const label =
                hint.keys.length === 1 ? hint.label : displayKeys[key] || key;
            const button = node("button", label, "key");
            button.type = "button";
            button.title = `${key}: ${hint.action}`;
            button.setAttribute("aria-label", `${key}: ${hint.action}`);
            button.addEventListener("pointerdown", (event) => event.preventDefault());
            button.addEventListener("click", () => action(index, choice, key));
            group.append(button);
        });
        group.append(node("span", hint.action, "action"));
        keys.append(group);
    });
    requestAnimationFrame(resize);
}

function paint(next) {
    frame = next;
    rememberUsername(next);

    if (next.error && pendingTab && !pendingTab.closed) {
        pendingTab.close();
        pendingTab = null;
    }

    screen.style.width = `${next.width * cellWidth}px`;
    screen.style.height = `${next.height * cellHeight}px`;
    const rows = Array.from({ length: next.height }, (_, index) => {
        const row = node("div", undefined, "terminal-row");
        row.style.top = `${index * cellHeight}px`;
        return row;
    });
    const textRows = Array.from({ length: next.height }, () => "");

    for (const run of next.runs) {
        const reverse = Boolean(run.modifiers & 64);
        const span = node("span", undefined, "terminal-run");
        span.style.left = `${run.x * cellWidth}px`;
        span.style.width = `${run.widths.reduce((total, width) => total + width, 0) * cellWidth}px`;
        span.style.color = reverse ? run.bg : run.fg;
        span.style.backgroundColor = reverse ? run.fg : run.bg;
        if (run.modifiers & 1) span.style.fontWeight = "bold";
        if (run.modifiers & 2) span.style.opacity = "0.65";
        if (run.modifiers & 4) span.style.fontStyle = "italic";
        if (run.modifiers & 128) span.style.visibility = "hidden";
        const decoration = [];
        if (run.modifiers & 8) decoration.push("underline");
        if (run.modifiers & 256) decoration.push("line-through");
        span.style.textDecorationLine = decoration.join(" ");

        if (run.widths.every(width => width === 1)) {
            span.textContent = run.text;
        } else {
            let index = 0;
            for (const { segment } of graphemes.segment(run.text)) {
                const cell = node("span", segment, "terminal-cell");
                cell.style.width = `${(run.widths[index++] || 1) * cellWidth}px`;
                span.append(cell);
            }
        }

        rows[run.y].append(span);
        textRows[run.y] += run.text;
    }

    screen.replaceChildren(...rows);

    accessible.textContent = textRows.map((row) => row.trimEnd()).join("\n");
    document.title = textRows[0]?.trim() || "Courses";
    hints(next.hints);

    if (!next.input && document.activeElement === input) input.blur();

    if (next.input && document.activeElement === input && !composing) {
        input.setAttribute("aria-label", next.input);
        resetInput();
    }
}

function disconnected(message) {
    frame = null;
    hintSignature = "";
    input.blur();
    pendingTab?.close();
    pendingTab = null;
    keys.replaceChildren(node("span", message + " "));
    const button = node("button", "Enter", "key");
    button.type = "button";
    button.addEventListener("click", connect);
    keys.append(button, node("span", " reconnect", "action"));
}

function openTarget(message) {
    if (message.type === "download") {
        const link = node("a");
        link.href = message.url;
        link.download = message.name || "";
        document.body.append(link);
        link.click();
        link.remove();

        return;
    }

    if (pendingTab && !pendingTab.closed) {
        pendingTab.location = message.url;
        pendingTab = null;

        return;
    }

    const tab = window.open(message.url, "_blank");

    if (tab) {
        tab.opener = null;

        return;
    }

    const link = node("a", "Open file / link");
    link.href = message.url;
    link.target = "_blank";
    link.rel = "noopener noreferrer";
    link.dataset.open = "true";
    keys.append(link);
}

async function connect() {
    if (
        connecting ||
        socket?.readyState === WebSocket.OPEN ||
        socket?.readyState === WebSocket.CONNECTING
    )
        return;

    connecting = true;
    socket = null;
    restoreUsername = true;
    lastUsername = null;
    keys.replaceChildren(node("span", "Connecting…"));
    lastSize = "";

    try {
        const response = await fetch("/api/bootstrap", {
            headers: { "X-Hello-Client": "1" },
            credentials: "same-origin",
        });

        if (!response.ok)
            throw new Error("Cannot start terminal. Reload the page.");

        if (response.headers.get("X-Hello-Transport") === "wasm") {
            const client = await import("./client.js");
            socket = await client.connect(await response.json(), message => {
                if (message.type === "frame") paint(message.frame);
                else if (["open", "download"].includes(message.type)) openTarget(message);
            }, disconnected);
            resize();

            return;
        }

        const url = new URL("/socket", location.href);
        url.protocol = location.protocol === "https:" ? "wss:" : "ws:";
        const connection = new WebSocket(url);
        socket = connection;
        connection.addEventListener("open", () => {
            if (socket === connection) resize();
        });
        connection.addEventListener("message", (event) => {
            if (socket !== connection) return;

            const message = JSON.parse(event.data);

            if (message.type === "frame") paint(message.frame);
            else if (["open", "download"].includes(message.type)) openTarget(message);
            else if (message.type === "closed") disconnected("Terminal closed.");
        });
        connection.addEventListener("close", () => {
            if (socket === connection) disconnected("Disconnected.");
        });
        connection.addEventListener("error", () => {
            if (socket === connection) disconnected("Connection failed.");
        });
    } catch (error) {
        disconnected(error.message);
    } finally {
        connecting = false;
    }
}

document.addEventListener("keydown", (event) => {
    if (event.target.closest?.("#site")) return;

    if (
        composing ||
        event.isComposing ||
        event.metaKey ||
        (event.ctrlKey && !["c", "C", "Home"].includes(event.key))
    )
        return;

    if (event.target.dataset?.open && event.key === "Enter") return;

    if (!frame) {
        if (event.key === "Enter") {
            event.preventDefault();
            connect();
        }

        return;
    }

    const special = [
        "ArrowUp",
        "ArrowDown",
        "ArrowLeft",
        "ArrowRight",
        "Enter",
        "Escape",
        "Backspace",
        "Delete",
        "Tab",
        "Home",
        "End",
        "PageUp",
        "PageDown",
        "F1",
        "F2",
        "F4",
        "F5",
    ];

    if (Array.from(event.key).length === 1 || special.includes(event.key)) {
        event.preventDefault();
        keyboard(event.key, event.ctrlKey, event.altKey, event.shiftKey);

        if (frame.input && !coarse) focusInput();
    }
});

input.addEventListener("beforeinput", (event) => {
    if (composing || event.isComposing) return;

    event.preventDefault();

    if (event.inputType === "insertCompositionText") return;

    if (event.inputType.startsWith("delete"))
        keyboard(event.inputType.includes("Forward") ? "Delete" : "Backspace");
    else if (event.inputType === "insertLineBreak") keyboard("Enter");
    else if (event.data) sendText(event.data);

    resetInput();
});

input.addEventListener("compositionstart", () => (composing = true));

input.addEventListener("compositionend", (event) => {
    composing = false;

    if (event.data) sendText(event.data);

    resetInput();
});

input.addEventListener("paste", (event) => {
    event.preventDefault();
    sendText(event.clipboardData.getData("text"));
    resetInput();
});

terminal.addEventListener("paste", (event) => {
    event.preventDefault();
    sendText(event.clipboardData.getData("text"));
});

function point(event) {
    const rect = screen.getBoundingClientRect();

    return {
        x: Math.max(0, Math.floor((event.clientX - rect.left) / cellWidth)),
        y: Math.max(0, Math.floor((event.clientY - rect.top) / cellHeight)),
    };
}

screen.addEventListener("pointerdown", (event) => {
    if (event.button !== 0) return;

    send({ type: "pointer", ...point(event), kind: "down" });

    if (frame?.input || frame?.mode === "login") focusInput(true);
    else terminal.focus({ preventScroll: true });
});

screen.addEventListener("pointermove", (event) => {
    if (event.pointerType !== "mouse" || pointerFrame) return;

    const position = point(event);
    pointerFrame = requestAnimationFrame(() => {
        pointerFrame = null;
        send({ type: "pointer", ...position, kind: "move" });
    });
});

screen.addEventListener("dblclick", (event) => {
    event.preventDefault();
    keyboard("Enter");
});

screen.addEventListener(
    "wheel",
    (event) => {
        event.preventDefault();
        send({
            type: "pointer",
            ...point(event),
            kind: event.deltaY > 0 ? "scroll_down" : "scroll_up",
        });
    },
    { passive: false },
);

new ResizeObserver(resize).observe(terminal);

window.addEventListener("resize", fitViewport);

window.visualViewport?.addEventListener("resize", fitViewport);

fitViewport();

connect();
