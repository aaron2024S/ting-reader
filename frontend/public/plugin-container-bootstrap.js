(function () {
  "use strict";

  var configMeta = document.querySelector('meta[name="ting-plugin-bootstrap"]');
  if (!configMeta) return;

  var config;
  try {
    config = JSON.parse(decodeURIComponent(configMeta.content || ""));
  } catch (_error) {
    return;
  } finally {
    configMeta.remove();
  }

  var bridgeToken = String(config.bridgeToken || "");
  var initialTheme = config.theme || {};
  var pluginAssetUrl = String(config.assetUrl || document.baseURI || "");
  if (!bridgeToken || !pluginAssetUrl) return;

  var channel = new MessageChannel();
  var bridgePort = channel.port1;
  var sendPort = bridgePort.postMessage.bind(bridgePort);
  var scheduleTask = window.setTimeout.bind(window);
  var lifecycleScript = document.currentScript;
  if (lifecycleScript) lifecycleScript.remove();
  var pluginListenersReady = document.readyState !== "loading";
  var externalNavigationPermit = false;
  var permitGeneration = 0;
  var queuedHostMessages = [];
  var deliverHostMessage = function (message) {
    window.postMessage(message, "*");
  };
  var applyTheme = function (theme) {
    var rawScheme = String(
      (theme && (theme.colorScheme || theme.brightness)) || "light",
    ).toLowerCase();
    var colorScheme = rawScheme.indexOf("dark") >= 0 ? "dark" : "light";
    var root = document.documentElement;
    var variables = (theme && theme.cssVariables) || {};
    root.style.setProperty("color-scheme", colorScheme, "important");
    root.dataset.tingTheme = colorScheme;
    root.dataset.theme = colorScheme;
    root.classList.toggle("dark", colorScheme === "dark");
    root.classList.toggle("light", colorScheme === "light");
    Object.keys(variables).forEach(function (name) {
      root.style.setProperty(name, String(variables[name]), "important");
    });
    if (document.body) {
      if (variables["--bg"]) {
        document.body.style.setProperty(
          "background-color",
          String(variables["--bg"]),
          "important",
        );
      }
      if (variables["--text"]) {
        document.body.style.setProperty(
          "color",
          String(variables["--text"]),
          "important",
        );
      }
    }
    window.__tingPluginTheme = theme;
  };

  Object.defineProperty(window, "__tingPluginApplyTheme", {
    value: applyTheme,
    configurable: false,
    enumerable: false,
    writable: false,
  });
  applyTheme(initialTheme);

  var markPluginListenersReady = function () {
    if (pluginListenersReady) return;
    pluginListenersReady = true;
    queuedHostMessages.splice(0).forEach(deliverHostMessage);
  };
  if (!pluginListenersReady) {
    document.addEventListener(
      "DOMContentLoaded",
      function () {
        scheduleTask(markPluginListenersReady, 0);
      },
      { capture: true, once: true },
    );
  }

  function createRequestError(payload, fallback) {
    var body = payload && typeof payload === "object" && !Array.isArray(payload) ? payload : {};
    var error = new Error(typeof body.message === "string" && body.message || fallback || "Request failed");
    error.name = "TingReaderError";
    error.code = typeof body.error === "string" ? body.error : "InternalError";
    error.details = body.details && typeof body.details === "object" && !Array.isArray(body.details)
      ? body.details : { retryable: false };
    error.retryable = error.details.retryable === true;
    error.trace_id = typeof body.trace_id === "string" ? body.trace_id : undefined;
    error.status = typeof body.status === "number" ? body.status : undefined;
    error.body = { error: error.code, message: error.message, details: error.details };
    if (error.trace_id) error.body.trace_id = error.trace_id;
    if (error.status) error.body.status = error.status;
    return error;
  }

  var postBridgeMessage = function (message) {
    sendPort(Object.assign({}, message, { bridge_token: bridgeToken }));
  };
  Object.defineProperty(window, "__TING_PLUGIN_BRIDGE__", {
    value: Object.freeze({
      createError: createRequestError,
      postMessage: function (message) {
        if (
          !message ||
          typeof message !== "object" ||
          message.type !== "ting-plugin:request"
        ) {
          return;
        }
        postBridgeMessage(message);
      },
    }),
    configurable: false,
    enumerable: false,
    writable: false,
  });

  bridgePort.addEventListener("message", function (event) {
    var message = event.data;
    if (!message || message.bridge_token !== bridgeToken) return;
    if (message.type === "ting-plugin:theme") applyTheme(message.theme);
    if (pluginListenersReady) {
      deliverHostMessage(message);
    } else if (queuedHostMessages.length < 128) {
      queuedHostMessages.push(message);
    }
  });
  bridgePort.start();

  window.addEventListener(
    "beforeunload",
    function () {
      postBridgeMessage({ type: "ting-plugin:document-unloading" });
    },
    { capture: true },
  );
  window.addEventListener("pagehide", function () {
    bridgePort.close();
  }, { capture: true, once: true });

  var absolutePluginUrl = function (url) {
    try {
      return new URL(url, pluginAssetUrl).href;
    } catch (_error) {
      return "";
    }
  };
  var isExternalUrl = function (url) {
    var absoluteUrl = absolutePluginUrl(url);
    if (!absoluteUrl) return false;
    try {
      var parsed = new URL(absoluteUrl);
      if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
        return false;
      }
      return (
        absoluteUrl !== pluginAssetUrl &&
        !absoluteUrl.startsWith(pluginAssetUrl + "#") &&
        !absoluteUrl.startsWith(pluginAssetUrl + "?")
      );
    } catch (_error) {
      return false;
    }
  };
  var grantExternalNavigation = function () {
    var generation = ++permitGeneration;
    externalNavigationPermit = true;
    scheduleTask(function () {
      if (permitGeneration === generation) externalNavigationPermit = false;
    }, 0);
  };
  var requestExternalNavigation = function (url) {
    if (!externalNavigationPermit) return;
    externalNavigationPermit = false;
    var absoluteUrl = absolutePluginUrl(url);
    if (!isExternalUrl(absoluteUrl)) return;
    postBridgeMessage({ type: "ting-plugin:external-url", url: absoluteUrl });
  };

  Object.defineProperty(window, "open", {
    value: function (url) {
      if (url && isExternalUrl(url)) requestExternalNavigation(url);
      return null;
    },
    configurable: false,
    writable: false,
  });
  window.addEventListener(
    "click",
    function (event) {
      if (event.isTrusted) grantExternalNavigation();
      var target = event.target;
      var anchor = target && target.closest ? target.closest("a[href]") : null;
      if (!anchor || !isExternalUrl(anchor.href)) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      requestExternalNavigation(anchor.href);
    },
    { capture: true },
  );

  window.parent.postMessage(
    {
      type: "ting-plugin:document-ready",
      bridge_token: bridgeToken,
    },
    "*",
    [channel.port2],
  );
})();
