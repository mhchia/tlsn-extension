import * as Comlink from "comlink";
import init, { initThreadPool, set_trace, notarize } from "./pkg/tlsn_extension_rs";

function hasSharedMemory() {
  const hasSharedArrayBuffer = "SharedArrayBuffer" in global;
  const notCrossOriginIsolated = global.crossOriginIsolated === false;

  return hasSharedArrayBuffer && !notCrossOriginIsolated;
}

// Configs
const NUM_LOOPS = 20;
const DATA_SIZE = 1  // KB
const NOTARY_MAX_TRANSCRIPT_SIZE = 49152;

const NOTARY_HOST = "localhost";
const NOTARY_PORT = 7047;
const WEBSOCKET_PROXY_BASE_URL = "ws://localhost:55688";
const USER_AGENT = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/114.0.0.0 Safari/537.36";
const serverDomain = "test-server.io";

console.log(`!@# benchmark: NUM_LOOPS=${NUM_LOOPS}, DATA_SIZE=${DATA_SIZE}KB`);

async function notarizeFixtureData() {
    // Can only be one of {1, 4, 8}
    const method = "GET";
    const url = `https://${serverDomain}/formats/json?size=${DATA_SIZE}`
    const body = new Uint8Array([])
    const headers = [
        ["Host", serverDomain],
        ["Accept", "*/*"],
        ["Accept-Encoding", "identity"],
        ["Connection", "close"],
        ["User-Agent", USER_AGENT],
    ]
    const websocketProxyURL = `${WEBSOCKET_PROXY_BASE_URL}?token=local`;
    const secrets = []
    const reveals = []

    const resProver = await notarize(
        NOTARY_MAX_TRANSCRIPT_SIZE,
        NOTARY_HOST,
        NOTARY_PORT,
        serverDomain,
        websocketProxyURL,
        method,
        url,
        headers,
        body,
        secrets,
        reveals,
    );
    return JSON.parse(resProver);
}


class Test {
    constructor() {
        console.log('!@# test comlink');
        this.test();
    }

    async test() {
        console.log('start');
        console.log("!@# hasSharedMemory=", hasSharedMemory())
        const numConcurrency = navigator.hardwareConcurrency;
        console.log("!@# numConcurrency=", numConcurrency)
        const res = await init();
        console.log("!@# res.memory=", res.memory)
        console.log("!@# res.memory.buffer.length=", res.memory.buffer.byteLength)
        await initThreadPool(numConcurrency);

        await set_trace();

        const runtimes = []
        for (let i = 0; i < NUM_LOOPS; i++) {
            const start = performance.now();
            const resJSON = await notarizeFixtureData();
            console.log("!@# res =", resJSON)
            const end = performance.now();
            runtimes.push(end - start);
        }
        console.log("!@# runtimes    =", runtimes.map(x => x / 1000))
        const avgRuntime = runtimes.reduce((a, b) => a + b, 0) / runtimes.length;
        console.log("!@# average time=", avgRuntime / 1000)
        // console.log("!@# resAfter.memory=", res.memory)
        // console.log("!@# resAfter.memory.buffer.length=", res.memory.buffer.byteLength)
    }
}

Comlink.expose(Test);