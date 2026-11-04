(() => {
let wasm_bindgen = (function(exports) {
    let script_src;
    if (typeof document !== 'undefined' && document.currentScript !== null) {
        script_src = new URL(document.currentScript.src, location.href).toString();
    }

    class WebRoom {
        static __wrap(ptr) {
            const obj = Object.create(WebRoom.prototype);
            obj.__wbg_ptr = ptr;
            WebRoomFinalization.register(obj, obj.__wbg_ptr, obj);
            return obj;
        }
        __destroy_into_raw() {
            const ptr = this.__wbg_ptr;
            this.__wbg_ptr = 0;
            WebRoomFinalization.unregister(this);
            return ptr;
        }
        free() {
            const ptr = this.__destroy_into_raw();
            wasm.__wbg_webroom_free(ptr, 0);
        }
        /**
         * @returns {string}
         */
        checkpoint_json() {
            let deferred2_0;
            let deferred2_1;
            try {
                const ret = wasm.webroom_checkpoint_json(this.__wbg_ptr);
                var ptr1 = ret[0];
                var len1 = ret[1];
                if (ret[3]) {
                    ptr1 = 0; len1 = 0;
                    throw takeFromExternrefTable0(ret[2]);
                }
                deferred2_0 = ptr1;
                deferred2_1 = len1;
                return getStringFromWasm0(ptr1, len1);
            } finally {
                wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
            }
        }
        /**
         * @param {string} key_hex
         * @param {string} nonce_hex
         * @param {string} plan_hash_hex
         * @returns {WebRoom}
         */
        static create(key_hex, nonce_hex, plan_hash_hex) {
            const ptr0 = passStringToWasm0(key_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(nonce_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            const ptr2 = passStringToWasm0(plan_hash_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len2 = WASM_VECTOR_LEN;
            const ret = wasm.webroom_create(ptr0, len0, ptr1, len1, ptr2, len2);
            if (ret[2]) {
                throw takeFromExternrefTable0(ret[1]);
            }
            return WebRoom.__wrap(ret[0]);
        }
        /**
         * @returns {string}
         */
        genesis_hex() {
            let deferred2_0;
            let deferred2_1;
            try {
                const ret = wasm.webroom_genesis_hex(this.__wbg_ptr);
                var ptr1 = ret[0];
                var len1 = ret[1];
                if (ret[3]) {
                    ptr1 = 0; len1 = 0;
                    throw takeFromExternrefTable0(ret[2]);
                }
                deferred2_0 = ptr1;
                deferred2_1 = len1;
                return getStringFromWasm0(ptr1, len1);
            } finally {
                wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
            }
        }
        /**
         * @param {string} action_json
         * @returns {string}
         */
        prepare(action_json) {
            let deferred3_0;
            let deferred3_1;
            try {
                const ptr0 = passStringToWasm0(action_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                const len0 = WASM_VECTOR_LEN;
                const ret = wasm.webroom_prepare(this.__wbg_ptr, ptr0, len0);
                var ptr2 = ret[0];
                var len2 = ret[1];
                if (ret[3]) {
                    ptr2 = 0; len2 = 0;
                    throw takeFromExternrefTable0(ret[2]);
                }
                deferred3_0 = ptr2;
                deferred3_1 = len2;
                return getStringFromWasm0(ptr2, len2);
            } finally {
                wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
            }
        }
        /**
         * @returns {string}
         */
        records_json() {
            let deferred2_0;
            let deferred2_1;
            try {
                const ret = wasm.webroom_records_json(this.__wbg_ptr);
                var ptr1 = ret[0];
                var len1 = ret[1];
                if (ret[3]) {
                    ptr1 = 0; len1 = 0;
                    throw takeFromExternrefTable0(ret[2]);
                }
                deferred2_0 = ptr1;
                deferred2_1 = len1;
                return getStringFromWasm0(ptr1, len1);
            } finally {
                wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
            }
        }
        /**
         * @param {string} genesis_hex
         * @param {string} records_json
         * @param {string} expected_head_hex
         * @returns {WebRoom}
         */
        static restore(genesis_hex, records_json, expected_head_hex) {
            const ptr0 = passStringToWasm0(genesis_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ptr1 = passStringToWasm0(records_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            const ptr2 = passStringToWasm0(expected_head_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len2 = WASM_VECTOR_LEN;
            const ret = wasm.webroom_restore(ptr0, len0, ptr1, len1, ptr2, len2);
            if (ret[2]) {
                throw takeFromExternrefTable0(ret[1]);
            }
            return WebRoom.__wrap(ret[0]);
        }
        /**
         * @returns {string}
         */
        settle_scenario() {
            let deferred2_0;
            let deferred2_1;
            try {
                const ret = wasm.webroom_settle_scenario(this.__wbg_ptr);
                var ptr1 = ret[0];
                var len1 = ret[1];
                if (ret[3]) {
                    ptr1 = 0; len1 = 0;
                    throw takeFromExternrefTable0(ret[2]);
                }
                deferred2_0 = ptr1;
                deferred2_1 = len1;
                return getStringFromWasm0(ptr1, len1);
            } finally {
                wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
            }
        }
        /**
         * @param {string} intent_hex
         * @param {string} signature_hex
         * @returns {string}
         */
        submit(intent_hex, signature_hex) {
            let deferred4_0;
            let deferred4_1;
            try {
                const ptr0 = passStringToWasm0(intent_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                const len0 = WASM_VECTOR_LEN;
                const ptr1 = passStringToWasm0(signature_hex, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                const len1 = WASM_VECTOR_LEN;
                const ret = wasm.webroom_submit(this.__wbg_ptr, ptr0, len0, ptr1, len1);
                var ptr3 = ret[0];
                var len3 = ret[1];
                if (ret[3]) {
                    ptr3 = 0; len3 = 0;
                    throw takeFromExternrefTable0(ret[2]);
                }
                deferred4_0 = ptr3;
                deferred4_1 = len3;
                return getStringFromWasm0(ptr3, len3);
            } finally {
                wasm.__wbindgen_free(deferred4_0, deferred4_1, 1);
            }
        }
        /**
         * @returns {string}
         */
        summary_json() {
            let deferred2_0;
            let deferred2_1;
            try {
                const ret = wasm.webroom_summary_json(this.__wbg_ptr);
                var ptr1 = ret[0];
                var len1 = ret[1];
                if (ret[3]) {
                    ptr1 = 0; len1 = 0;
                    throw takeFromExternrefTable0(ret[2]);
                }
                deferred2_0 = ptr1;
                deferred2_1 = len1;
                return getStringFromWasm0(ptr1, len1);
            } finally {
                wasm.__wbindgen_free(deferred2_0, deferred2_1, 1);
            }
        }
    }
    if (Symbol.dispose) WebRoom.prototype[Symbol.dispose] = WebRoom.prototype.free;
    exports.WebRoom = WebRoom;

    /**
     * @param {string} action_json
     * @returns {string}
     */
    function preview_allocation(action_json) {
        let deferred3_0;
        let deferred3_1;
        try {
            const ptr0 = passStringToWasm0(action_json, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len0 = WASM_VECTOR_LEN;
            const ret = wasm.preview_allocation(ptr0, len0);
            var ptr2 = ret[0];
            var len2 = ret[1];
            if (ret[3]) {
                ptr2 = 0; len2 = 0;
                throw takeFromExternrefTable0(ret[2]);
            }
            deferred3_0 = ptr2;
            deferred3_1 = len2;
            return getStringFromWasm0(ptr2, len2);
        } finally {
            wasm.__wbindgen_free(deferred3_0, deferred3_1, 1);
        }
    }
    exports.preview_allocation = preview_allocation;
    function __wbg_get_imports() {
        const import0 = {
            __proto__: null,
            __wbg___wbindgen_throw_344f42d3211c4765: function(arg0, arg1) {
                throw new Error(getStringFromWasm0(arg0, arg1));
            },
            __wbindgen_cast_0000000000000001: function(arg0, arg1) {
                // Cast intrinsic for `Ref(String) -> Externref`.
                const ret = getStringFromWasm0(arg0, arg1);
                return ret;
            },
            __wbindgen_init_externref_table: function() {
                const table = wasm.__wbindgen_externrefs;
                const offset = table.grow(4);
                table.set(0, undefined);
                table.set(offset + 0, undefined);
                table.set(offset + 1, null);
                table.set(offset + 2, true);
                table.set(offset + 3, false);
            },
        };
        return {
            __proto__: null,
            "./sakura_rooms_bg.js": import0,
        };
    }

    const WebRoomFinalization = (typeof FinalizationRegistry === 'undefined')
        ? { register: () => {}, unregister: () => {} }
        : new FinalizationRegistry(ptr => wasm.__wbg_webroom_free(ptr, 1));

    function getStringFromWasm0(ptr, len) {
        return decodeText(ptr >>> 0, len);
    }

    let cachedUint8ArrayMemory0 = null;
    function getUint8ArrayMemory0() {
        if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
            cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
        }
        return cachedUint8ArrayMemory0;
    }

    function passStringToWasm0(arg, malloc, realloc) {
        if (realloc === undefined) {
            const buf = cachedTextEncoder.encode(arg);
            const ptr = malloc(buf.length, 1) >>> 0;
            getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
            WASM_VECTOR_LEN = buf.length;
            return ptr;
        }

        let len = arg.length;
        let ptr = malloc(len, 1) >>> 0;

        const mem = getUint8ArrayMemory0();

        let offset = 0;

        for (; offset < len; offset++) {
            const code = arg.charCodeAt(offset);
            if (code > 0x7F) break;
            mem[ptr + offset] = code;
        }
        if (offset !== len) {
            if (offset !== 0) {
                arg = arg.slice(offset);
            }
            ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
            const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
            const ret = cachedTextEncoder.encodeInto(arg, view);

            offset += ret.written;
            ptr = realloc(ptr, len, offset, 1) >>> 0;
        }

        WASM_VECTOR_LEN = offset;
        return ptr;
    }

    function takeFromExternrefTable0(idx) {
        const value = wasm.__wbindgen_externrefs.get(idx);
        wasm.__externref_table_dealloc(idx);
        return value;
    }

    let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
    cachedTextDecoder.decode();
    function decodeText(ptr, len) {
        return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
    }

    const cachedTextEncoder = new TextEncoder();

    if (!('encodeInto' in cachedTextEncoder)) {
        cachedTextEncoder.encodeInto = function (arg, view) {
            const buf = cachedTextEncoder.encode(arg);
            view.set(buf);
            return {
                read: arg.length,
                written: buf.length
            };
        };
    }

    let WASM_VECTOR_LEN = 0;

    let wasmModule, wasmInstance, wasm;
    function __wbg_finalize_init(instance, module) {
        wasmInstance = instance;
        wasm = instance.exports;
        wasmModule = module;
        cachedUint8ArrayMemory0 = null;
        wasm.__wbindgen_start();
        return wasm;
    }

    async function __wbg_load(module, imports) {
        if (typeof Response === 'function' && module instanceof Response) {
            if (typeof WebAssembly.instantiateStreaming === 'function') {
                try {
                    return await WebAssembly.instantiateStreaming(module, imports);
                } catch (e) {
                    const validResponse = module.ok && expectedResponseType(module.type);

                    if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                        console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                    } else { throw e; }
                }
            }

            const bytes = await module.arrayBuffer();
            return await WebAssembly.instantiate(bytes, imports);
        } else {
            const instance = await WebAssembly.instantiate(module, imports);

            if (instance instanceof WebAssembly.Instance) {
                return { instance, module };
            } else {
                return instance;
            }
        }

        function expectedResponseType(type) {
            switch (type) {
                case 'basic': case 'cors': case 'default': return true;
            }
            return false;
        }
    }

    function initSync(module) {
        if (wasm !== undefined) return wasm;


        if (module !== undefined) {
            if (Object.getPrototypeOf(module) === Object.prototype) {
                ({module} = module)
            } else {
                console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
            }
        }

        const imports = __wbg_get_imports();
        if (!(module instanceof WebAssembly.Module)) {
            module = new WebAssembly.Module(module);
        }
        const instance = new WebAssembly.Instance(module, imports);
        return __wbg_finalize_init(instance, module);
    }

    async function __wbg_init(module_or_path) {
        if (wasm !== undefined) return wasm;


        if (module_or_path !== undefined) {
            if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
                ({module_or_path} = module_or_path)
            } else {
                console.warn('using deprecated parameters for the initialization function; pass a single object instead')
            }
        }

        if (module_or_path === undefined && script_src !== undefined) {
            module_or_path = script_src.replace(/\.js$/, "_bg.wasm");
        }
        const imports = __wbg_get_imports();

        if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
            module_or_path = fetch(module_or_path);
        }

        const { instance, module } = await __wbg_load(await module_or_path, imports);

        return __wbg_finalize_init(instance, module);
    }

    return Object.assign(__wbg_init, { initSync }, exports);
})({ __proto__: null });

globalThis.sakura_rooms_engine = wasm_bindgen;
})();
