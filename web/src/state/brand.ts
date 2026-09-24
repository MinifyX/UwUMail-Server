import { create } from "zustand";

/** What the server is called and how it looks; UwUMail unless an admin chose otherwise. */
export interface Brand {
  name: string;
  /** Whether anything differs from UwUMail as it comes. */
  custom: boolean;
  /** The accent colour as `#rrggbb`; `null` for the UwUMail pink. */
  color: string | null;
  /** Whether Nyu the cat and the playful tone appear. */
  mascot: boolean;
  /** Where the uploaded logo is, with its version; `null` for Nyu. */
  logo: string | null;
}

export const DEFAULT_BRAND: Brand = { name: "UwUMail", custom: false, color: null, mascot: true, logo: null };

interface BrandState extends Brand {
  apply: (brand: Partial<Brand> | null | undefined) => void;
}

/** Filled from `/api/info` and `/api/session`, which both carry the brand. */
export const useBrand = create<BrandState>((set) => ({
  ...DEFAULT_BRAND,
  apply: (brand) => set(() => ({ ...DEFAULT_BRAND, ...(brand ?? {}) })),
}));
