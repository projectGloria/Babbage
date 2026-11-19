import { useEffect } from "react";

const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * Keeps Tab/Shift+Tab cycling within a modal container and restores focus
 * to the previously focused element when the modal closes.
 */
export function useFocusTrap(
  containerRef: React.RefObject<HTMLElement | null>,
  active = true,
  autoFocus = true,
) {
  useEffect(() => {
    if (!active) return;
    const container = containerRef.current;
    if (!container) return;

    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const focusables = () =>
      Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
        (el) => !el.hasAttribute("disabled") && el.getAttribute("aria-hidden") !== "true",
      );

    if (autoFocus && !container.contains(document.activeElement)) {
      const first = focusables()[0];
      if (first) {
        first.focus();
      } else {
        container.setAttribute("tabindex", "-1");
        container.focus();
      }
    }

    const trapTab = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const list = focusables();
      if (list.length === 0) {
        e.preventDefault();
        e.stopPropagation();
        return;
      }
      const first = list[0];
      const last = list[list.length - 1];
      const cur = document.activeElement;
      if (e.shiftKey) {
        if (cur === first || cur === container || !container.contains(cur)) {
          e.preventDefault();
          e.stopPropagation();
          last.focus();
        }
      } else if (cur === last || !container.contains(cur)) {
        e.preventDefault();
        e.stopPropagation();
        first.focus();
      }
    };

    container.addEventListener("keydown", trapTab);
    window.addEventListener("keydown", trapTab);
    return () => {
      container.removeEventListener("keydown", trapTab);
      window.removeEventListener("keydown", trapTab);
      if (previous && document.contains(previous)) {
        previous.focus();
      }
    };
  }, [containerRef, active, autoFocus]);
}
