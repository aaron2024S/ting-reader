//! JavaScript init code for plugin runtime
//!
//! Generates the initialization JavaScript that sets up the Ting API globals
//! (Ting, fetch, Headers/URL polyfills, _ting_invoke) in the Deno runtime.

use serde_json::Value;

/// Generate the JavaScript init code that bootstraps the Ting environment
pub fn generate_init_code(
    plugin_name: &str,
    config: &Value,
    allowed_paths: &[String],
    allowed_domains: &[String],
) -> String {
    let config_json = serde_json::to_string(config).unwrap_or_else(|_| "{}".to_string());
    let paths_json = serde_json::to_string(allowed_paths).unwrap_or_else(|_| "[]".to_string());
    let domains_json = serde_json::to_string(allowed_domains).unwrap_or_else(|_| "[]".to_string());

    format!(
        r#"
        "use strict";

        // Polyfill Headers
        globalThis.Headers = class Headers {{
            constructor(init) {{
                this.map = new Map();
                if (init) {{
                    if (init instanceof Headers) {{
                        init.forEach((value, key) => this.append(key, value));
                    }} else if (Array.isArray(init)) {{
                        init.forEach(([key, value]) => this.append(key, value));
                    }} else {{
                        Object.keys(init).forEach(key => this.append(key, init[key]));
                    }}
                }}
            }}
            append(name, value) {{
                name = name.toLowerCase();
                value = String(value);
                if (this.map.has(name)) {{
                    this.map.get(name).push(value);
                }} else {{
                    this.map.set(name, [value]);
                }}
            }}
            delete(name) {{ this.map.delete(name.toLowerCase()); }}
            get(name) {{
                const values = this.map.get(name.toLowerCase());
                return values ? values[0] : null;
            }}
            has(name) {{ return this.map.has(name.toLowerCase()); }}
            set(name, value) {{ this.map.set(name.toLowerCase(), [String(value)]); }}
            forEach(callback, thisArg) {{
                for (const [name, values] of this.map) {{
                    callback.call(thisArg, values.join(', '), name, this);
                }}
            }}
        }};

        // Polyfill URL (Minimal)
        globalThis.URL = class URL {{
            constructor(url, base) {{
                if (base) {{
                    if (base.endsWith('/')) base = base.slice(0, -1);
                    if (!url.startsWith('/')) url = '/' + url;
                    url = base + url;
                }}
                this.href = url;
                const match = url.match(/^(https?:)\/\/([^/?#]+)(.*)$/);
                if (match) {{
                    this.protocol = match[1];
                    this.hostname = match[2];
                    this.pathname = match[3] || '/';
                    this.search = '';
                    if (this.pathname.includes('?')) {{
                        const parts = this.pathname.split('?');
                        this.pathname = parts[0];
                        this.search = '?' + parts[1];
                    }}
                }} else {{
                    this.hostname = '';
                    this.protocol = '';
                    this.pathname = '';
                    this.search = '';
                }}
            }}
            toString() {{ return this.href; }}
        }};

        function __tingFormatLogValue(value) {{
            if (typeof value === "string") return value;
            if (value === undefined) return "undefined";
            try {{
                const encoded = JSON.stringify(value);
                return encoded === undefined ? String(value) : encoded;
            }} catch (_) {{
                return String(value);
            }}
        }}

        function __tingWriteLog(level, message, fields) {{
            const normalizedFields = fields === undefined || fields === null ? null : fields;
            if (
                normalizedFields !== null &&
                (typeof normalizedFields !== "object" || Array.isArray(normalizedFields))
            ) {{
                throw new TypeError("Ting.log fields must be an object");
            }}
            return Deno.core.ops.op_plugin_log(
                level,
                __tingFormatLogValue(message),
                normalizedFields,
            );
        }}

        function __tingWriteConsole(level, args) {{
            return __tingWriteLog(level, args.map(__tingFormatLogValue).join(" "), null);
        }}

        // Ting Plugin API for JavaScript
        globalThis.Ting = {{
            pluginName: "{plugin_name}",
            config: {config_json},

            resources: Object.freeze({{
                invoke: (operation, input = {{}}) => Deno.core.ops.op_host_invoke("resources." + operation, input),
                chunkCreate: (bytes) => Deno.core.ops.op_chunk_create(bytes),
                chunkCopy: (id) => Deno.core.ops.op_chunk_copy(id),
                writeAt: (resource, offset, bytes) => Deno.core.ops.op_resource_write(resource, offset, bytes),
            }}),

            // Sandbox information
            sandbox: {{
                allowedPaths: {paths_json},
                allowedDomains: {domains_json},
            }},

            // Logging functions
            log: {{
                debug: (message, fields) => __tingWriteLog("debug", message, fields),
                info: (message, fields) => __tingWriteLog("info", message, fields),
                warn: (message, fields) => __tingWriteLog("warn", message, fields),
                error: (message, fields) => __tingWriteLog("error", message, fields),
            }},

            // Configuration access
            getConfig: (key) => {{
                const config = globalThis.Ting?.config || {{}};
                return config[key] ?? null;
            }},

            // Events are delivered through the declared Host event gateway.
            events: {{
                publish: async (eventType, data) =>
                    await Ting.host.invoke("events.publish", {{
                        event: eventType,
                        data: data ?? null,
                    }}),
                subscribe: () => {{
                    throw new Error(
                        "Dynamic event subscriptions are unavailable; declare an event_handler capability",
                    );
                }},
            }},

            // Host context access. Server-side JS plugins receive a per-invocation
            // context when the caller goes through capability/plugin-route bridges.
            host: {{
                getContext: () => globalThis._ting_context || null,
                invoke: async (method, params = {{}}) => {{
                    if (!method || typeof method !== 'string') {{
                        throw new Error("Ting.host.invoke requires a method string");
                    }}
                    return await Deno.core.ops.op_host_invoke(method, params ?? {{}});
                }},
            }},

        }};

        // Preserve familiar console APIs while binding log identity in the host.
        globalThis.console.log = (...args) => __tingWriteConsole("info", args);
        globalThis.console.debug = (...args) => __tingWriteConsole("debug", args);
        globalThis.console.warn = (...args) => __tingWriteConsole("warn", args);
        globalThis.console.error = (...args) => __tingWriteConsole("error", args);

        function safeNetworkTarget(url) {{
            try {{
                const parsed = new URL(url);
                return parsed.protocol + '//' + parsed.hostname;
            }} catch (_) {{
                return '<invalid-url>';
            }}
        }}

        // fetch uses the same authoritative Host HTTP service as Rust SDKs.
        globalThis.fetch = async function(url, options = {{}}) {{
            const result = await Ting.host.invoke("http.request", {{
                url: typeof url === 'string' ? url : url.toString(),
                method: options.method || 'GET',
                headers: options.headers || {{}},
                body: options.body ?? null,
                timeout_ms: options.timeout_ms ?? 30000,
            }});
            let bytesPromise;
            const readBytes = () => bytesPromise ||= (async () => {{
                if (!Number.isSafeInteger(result.length) || result.length < 0 || result.length > 8 * 1024 * 1024) {{
                    throw new RangeError('Invalid HTTP response length');
                }}
                const bytes = new Uint8Array(result.length);
                let offset = 0;
                try {{
                    while (offset < bytes.length) {{
                        const read = await Ting.resources.invoke('read_at', {{
                            resource: result.resource, offset,
                            max_bytes: Math.min(256 * 1024, bytes.length - offset),
                        }});
                        try {{
                            const chunk = Ting.resources.chunkCopy(read.chunk);
                            if (!chunk.length || chunk.length !== read.bytes || chunk.length > bytes.length - offset) {{
                                throw new Error('HTTP response was truncated');
                            }}
                            bytes.set(chunk, offset);
                            offset += chunk.length;
                        }} finally {{
                            await Ting.resources.invoke('release_chunk', {{ chunk: read.chunk }});
                        }}
                    }}
                    return bytes;
                }} finally {{
                    await Ting.resources.invoke('close', {{ resource: result.resource }});
                }}
            }})();
            return {{
                ok: result.status >= 200 && result.status < 300,
                status: result.status,
                headers: new Headers(result.headers),
                text: async () => Deno.core.ops.op_decode_utf8(await readBytes()),
                json: async () => JSON.parse(Deno.core.ops.op_decode_utf8(await readBytes())),
                arrayBuffer: async () => (await readBytes()).buffer,
            }};
        }};

        // Helper for invoking functions from Rust without recompiling scripts
        globalThis._ting_invoke = async function(funcName, args) {{
            try {{
                globalThis._ting_status = 'pending';
                globalThis._ting_result = undefined;
                globalThis._ting_error = undefined;
                globalThis._ting_context = args && typeof args === 'object' ? (args._context || null) : null;

                const func = globalThis[funcName];
                if (typeof func !== 'function') {{
                    throw new Error(`Function ${{funcName}} not found`);
                }}

                const result = await func(args);
                globalThis._ting_result = JSON.stringify(result);
                globalThis._ting_status = 'success';
            }} catch (e) {{
                globalThis._ting_error = e.toString();
                globalThis._ting_status = 'error';
            }} finally {{
                globalThis._ting_context = undefined;
            }}
        }};
        "#,
        plugin_name = plugin_name,
        config_json = config_json,
        paths_json = paths_json,
        domains_json = domains_json,
    )
}
