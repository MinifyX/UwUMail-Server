import { useEffect } from "react";
import { useApplyBrandName } from "@/i18n";
import { useBrand } from "@/state/brand";

const DEFAULT_ICON = "/uwumail-app-icon.svg";

/**
 * Carries the brand into what lives outside React: the page title, the icon in the browser tab,
 * `{{brand}}` in the texts, and the stylesheet with the colours (index.html links /branding.css).
 */
export function useApplyBrand() {
  const name = useBrand((s) => s.name);
  const logo = useBrand((s) => s.logo);
  const color = useBrand((s) => s.color);
  useApplyBrandName();

  useEffect(() => {
    document.title = name;
  }, [name]);

  useEffect(() => {
    const icon = document.querySelector<HTMLLinkElement>("link[rel='icon']");
    if (!icon) return;
    icon.href = logo ?? DEFAULT_ICON;
    icon.type = logo ? "" : "image/svg+xml";
  }, [logo]);

  // The stylesheet was loaded with the page; a colour chosen since needs it fetched again.
  useEffect(() => {
    const sheet = document.querySelector<HTMLLinkElement>("link#brand-css");
    if (!sheet) return;
    const url = new URL(sheet.href, window.location.href);
    const version = color ?? "";
    if (url.searchParams.get("c") === version) return;
    url.searchParams.set("c", version);
    sheet.href = url.pathname + url.search;
  }, [color]);
}
