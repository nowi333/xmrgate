// Shared helpers: text lookup, requests, clipboard.
(function () {
  const lang = "en";

  function translator(dict) {
    const t = (key, vars) => {
      let text = dict.en[key] || key;
      for (const name in vars || {}) text = text.split("{" + name + "}").join(vars[name]);
      return text;
    };
    t.apply = (root) => {
      (root || document).querySelectorAll("[data-t]").forEach((el) => { el.textContent = t(el.dataset.t); });
      (root || document).querySelectorAll("[data-t-placeholder]").forEach((el) => { el.placeholder = t(el.dataset.tPlaceholder); });
    };
    return t;
  }

  async function request(method, url, body) {
    const response = await fetch(url, {
      method,
      headers: body ? { "Content-Type": "application/json" } : {},
      body: body ? JSON.stringify(body) : undefined,
      credentials: "same-origin",
    });
    let data = {};
    try { data = await response.json(); } catch (e) { /* empty body */ }
    return { ok: response.ok, status: response.status, data };
  }

  let toastTimer;
  function toast(text) {
    let el = document.querySelector(".toast");
    if (!el) {
      el = document.createElement("div");
      el.className = "toast";
      el.setAttribute("role", "status");
      document.body.appendChild(el);
    }
    el.textContent = text;
    el.classList.remove("hidden");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => el.classList.add("hidden"), 1800);
  }

  async function copy(text, message) {
    try {
      await navigator.clipboard.writeText(text);
    } catch (e) {
      const area = document.createElement("textarea");
      area.value = text;
      document.body.appendChild(area);
      area.select();
      try { document.execCommand("copy"); } catch (e2) { /* nothing more to try */ }
      area.remove();
    }
    toast(message);
  }

  window.xg = {
    lang,
    translator,
    get: (url) => request("GET", url),
    post: (url, body) => request("POST", url, body || {}),
    copy,
    toast,
    el: (id) => document.getElementById(id),
  };
})();
