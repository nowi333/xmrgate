(function () {
  const el = xg.el;
  const base = location.pathname.replace(/\/$/, "");

  xg.get(base + "/info").then(({ ok, data }) => {
    if (!ok) return;
    el("shop").textContent = data.shop_name || "Monero payment";
    el("name").textContent = data.name;
    el("price").textContent = "";
    el("price").append(data.amount + " ");
    const unit = document.createElement("small");
    unit.textContent = data.currency;
    el("price").append(unit);
    el("rate-note").textContent = data.currency === "XMR" ? "" : "Paid in Monero (XMR) at the current rate.";
    document.title = data.name;
  });

  // Submitted with fetch so a refusal can be explained in place. Without JavaScript the plain
  // form post still works.
  el("form").addEventListener("submit", async (event) => {
    event.preventDefault();
    const button = el("submit");
    button.disabled = true;
    const response = await fetch(base, { method: "POST", redirect: "follow" });
    if (response.ok && response.redirected) {
      location.href = response.url;
      return;
    }
    button.disabled = false;
    const error = el("error");
    error.textContent = response.status === 429
      ? "Many people are paying right now. Please try again in a few minutes."
      : "Monero payments are unavailable right now. Please try again in a moment.";
    error.classList.remove("hidden");
  });
})();
