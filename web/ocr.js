import * as ort from "onnxruntime-web/wasm";

ort.env.wasm.numThreads = 1;
ort.env.wasm.proxy = false;
ort.env.wasm.wasmPaths = new URL("./ort/", location.href).href;

const alphabet = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
let glyphModel, wholeModel;

async function model(name) {
    const url = new URL(`./assets/${name}`, location.href).href;
    return ort.InferenceSession.create(url, { executionProviders: ["wasm"] });
}

function scores(values) {
    let best = 0;
    for (let i = 1; i < values.length; i++) {
        if (values[i] > values[best]) best = i;
    }
    let sum = 0;
    for (const value of values) sum += Math.exp(value - values[best]);
    return { character: alphabet[best], confidence: 1 / sum };
}

export async function predict(glyphs, whole) {
    try {
        if (glyphs.length === 5) {
            glyphModel ||= model("captcha-glyph.onnx");
            const session = await glyphModel;
            let text = "", confidence = 1;
            for (const glyph of glyphs) {
                const input = Float32Array.from(glyph, value => value / 255);
                const output = await session.run({ [session.inputNames[0]]: new ort.Tensor("float32", input, [1, 1, 32, 32]) });
                const result = scores(output[session.outputNames[0]].data);
                text += result.character;
                confidence = Math.min(confidence, result.confidence);
            }
            return { text, confidence };
        }
    } catch (_) {
        glyphModel = undefined;
    }

    wholeModel ||= model("captcha-cnn.onnx");
    const session = await wholeModel;
    const input = Float32Array.from(whole, value => value / 255);
    const output = await session.run({ [session.inputNames[0]]: new ort.Tensor("float32", input, [1, 1, 64, 160]) });
    const values = output[session.outputNames[0]].data;
    let text = "", confidence = 1;
    for (let i = 0; i < 5; i++) {
        const result = scores(values.subarray(i * 62, (i + 1) * 62));
        text += result.character;
        confidence = Math.min(confidence, result.confidence);
    }
    return { text, confidence };
}
