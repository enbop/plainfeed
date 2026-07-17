(() => {
  const observed = new WeakSet();

  const observer = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting || entry.intersectionRatio < 0.6) continue;
      const card = entry.target;
      observer.unobserve(card);
      window.setTimeout(async () => {
        if (!card.isConnected) return;
        try {
          const response = await fetch(`/entries/${card.dataset.entryId}/read`, {
            method: "POST",
            headers: { "Content-Type": "application/x-www-form-urlencoded" },
          });
          if (response.ok) {
            card.dataset.unread = "false";
            card.classList.remove("is-unread");
          }
        } catch (error) {
          console.warn("Plainfeed could not mark the entry as read", error);
        }
      }, 900);
    }
  }, { threshold: [0.6] });

  function observeUnread(root = document) {
    const selector = ".entry-card[data-unread='true']";
    const cards = [
      ...(root.matches?.(selector) ? [root] : []),
      ...root.querySelectorAll(selector),
    ];
    for (const card of cards) {
      if (observed.has(card)) continue;
      observed.add(card);
      observer.observe(card);
    }
  }

  document.addEventListener("DOMContentLoaded", () => observeUnread());
  document.addEventListener("htmx:afterSwap", (event) => observeUnread(event.target));
})();
