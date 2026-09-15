import type { ReactNode } from "react";

/**
 * Nyu's palette. The mascot is artwork with fixed colors: it looks the same in
 * both themes, and a white sticker edge keeps it readable on dark backgrounds.
 */
export const NYU = {
  ink: "#4B1D3F",
  body: "#FF6FA6",
  flap: "#FFB8D3",
  blush: "#FF7FB0",
  paper: "#FFFFFF",
  star: "#FFD66E",
  tear: "#9ED8FF",
  lilac: "#CDB8FF",
  violet: "#A78BFA",
  mint: "#B9F0D0",
  sky: "#BDE6FF",
  kraft: "#F2C58F",
  kraftLight: "#F8DDB8",
  cloud: "#ECE6F4",
} as const;

export type NyuMood = "uwu" | "sleepy" | "happy" | "cheer" | "sparkle" | "sad" | "puzzled";

/** Draws its children twice: first as a white die-cut edge, then as they are. */
export function Sticker({ edge, children }: { edge: number; children: ReactNode }) {
  return (
    <>
      <g className="nyu-edge" strokeWidth={edge}>
        {children}
      </g>
      {children}
    </>
  );
}

const line = { fill: "none", stroke: NYU.ink, strokeWidth: 14 } as const;

const EYES: Record<NyuMood, ReactNode> = {
  uwu: (
    <g {...line}>
      <path d="M210 194 v4 a12 12 0 0 0 24 0 v-4" />
      <path d="M278 194 v4 a12 12 0 0 0 24 0 v-4" />
    </g>
  ),
  sleepy: (
    <g {...line}>
      <path d="M208 202 q14 10 28 0" />
      <path d="M276 202 q14 10 28 0" />
    </g>
  ),
  happy: (
    <g>
      <g fill={NYU.ink}>
        <ellipse cx="222" cy="202" rx="10" ry="13" />
        <ellipse cx="290" cy="202" rx="10" ry="13" />
      </g>
      <g fill={NYU.paper}>
        <circle cx="226" cy="196" r="4" />
        <circle cx="294" cy="196" r="4" />
      </g>
    </g>
  ),
  cheer: (
    <g {...line}>
      <path d="M212 190 L231 201 L212 212" />
      <path d="M300 190 L281 201 L300 212" />
    </g>
  ),
  sparkle: (
    <g fill={NYU.star} stroke={NYU.ink} strokeWidth={5}>
      <path d="M222 184 Q225 199 240 202 Q225 205 222 220 Q219 205 204 202 Q219 199 222 184Z" />
      <path d="M290 184 Q293 199 308 202 Q293 205 290 220 Q287 205 272 202 Q287 199 290 184Z" />
    </g>
  ),
  sad: (
    <g>
      <g {...line}>
        <path d="M208 208 q14 -12 28 0" />
        <path d="M276 208 q14 -12 28 0" />
      </g>
      <g fill={NYU.tear} stroke={NYU.ink} strokeWidth={5}>
        <path d="M214 214 q-8 12 0 17 q8 -5 0 -17Z" />
        <path d="M298 214 q-8 12 0 17 q8 -5 0 -17Z" />
      </g>
    </g>
  ),
  puzzled: (
    <g fill={NYU.ink}>
      <circle cx="222" cy="202" r="8" />
      <circle cx="290" cy="202" r="8" />
    </g>
  ),
};

const W_MOUTH = <path d="M236 230 q10 13 20 0 q10 13 20 0" {...line} />;

const MOUTHS: Record<NyuMood, ReactNode> = {
  uwu: W_MOUTH,
  happy: W_MOUTH,
  sparkle: W_MOUTH,
  sleepy: <path d="M244 234 q6 8 12 0 q6 8 12 0" {...line} strokeWidth={12} />,
  cheer: (
    <g>
      <path d="M238 226 Q256 258 274 226 Z" fill={NYU.ink} stroke={NYU.ink} strokeWidth={10} />
      <ellipse cx="256" cy="242" rx="8" ry="5" fill={NYU.blush} />
    </g>
  ),
  sad: <path d="M240 242 Q256 228 272 242" {...line} strokeWidth={12} />,
  puzzled: (
    <g>
      <path d="M242 236 q7 -6 14 0 q7 6 14 0" {...line} strokeWidth={11} />
      <path d="M350 180 q-10 14 0 21 q10 -7 0 -21Z" fill={NYU.tear} stroke={NYU.ink} strokeWidth={5} />
    </g>
  ),
};

function Ear({ side }: { side: "l" | "r" }) {
  return (
    <g className={`nyu-ear nyu-ear-${side}`}>
      <g transform={side === "r" ? "matrix(-1 0 0 1 512 0)" : undefined}>
        <path d="M118 200 L132 100 Q135 82 151 91 L230 156 Z" fill={NYU.body} stroke={NYU.ink} strokeWidth={14} />
        <path d="M146 150 L151 116 Q152 108 159 112 L196 144 Z" fill={NYU.flap} />
      </g>
    </g>
  );
}

/** A paw in Nyu's own coordinates. */
export function Paw({ x, y, className }: { x: number; y: number; className?: string }) {
  return (
    <g className={className}>
      <ellipse cx={x} cy={y} rx="30" ry="25" fill={NYU.body} stroke={NYU.ink} strokeWidth={14} />
      <path d={`M${x - 8} ${y + 4} v10 M${x + 8} ${y + 4} v10`} fill="none" stroke={NYU.ink} strokeWidth={8} />
    </g>
  );
}

interface NyuProps {
  mood?: NyuMood;
  /** Centre of the body in the parent's coordinates. */
  x?: number;
  y?: number;
  /** 1 is the app icon's size: the body is 312 wide. */
  scale?: number;
  tilt?: number;
  /** Extra parts in Nyu's own coordinates, where the body spans 100–412 × 152–378. */
  behind?: ReactNode;
  front?: ReactNode;
}

/** Nyu, the envelope cat. Its flap is the face, the ears poke out on top. */
export function Nyu({ mood = "uwu", x = 256, y = 265, scale = 1, tilt = 0, behind, front }: NyuProps) {
  return (
    <g
      transform={`translate(${x} ${y}) rotate(${tilt}) scale(${scale}) translate(-256 -265)`}
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <Sticker edge={44}>
        {behind}
        <Ear side="l" />
        <Ear side="r" />
        <rect x="100" y="152" width="312" height="226" rx="50" fill={NYU.body} />
        <path d="M100 202 A50 50 0 0 1 150 152 H362 A50 50 0 0 1 412 202 L276 290 Q256 304 236 290 Z" fill={NYU.flap} />
        <rect x="100" y="152" width="312" height="226" rx="50" {...line} />
        <path d="M108 207 L236 290 Q256 304 276 290 L404 207" {...line} />
        <g transform="translate(256 216) scale(1.2) translate(-256 -216)">
          {mood !== "puzzled" && (
            <g fill={NYU.blush}>
              <ellipse cx="194" cy="238" rx="15" ry="8.5" />
              <ellipse cx="318" cy="238" rx="15" ry="8.5" />
            </g>
          )}
          <g className={mood === "sleepy" ? undefined : "nyu-eyes"}>{EYES[mood]}</g>
          {MOUTHS[mood]}
        </g>
        {front}
      </Sticker>
    </g>
  );
}
