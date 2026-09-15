import clsx from "clsx";
import type { ReactNode } from "react";
import { Nyu, NYU, Paw, Sticker } from "./Nyu";

// Every scene is drawn on a 320 × 220 canvas. Nyu sits at about 0.4 scale,
// so props use a 6 px outline and an 18 px edge to match it.

const S = { stroke: NYU.ink, strokeWidth: 6 } as const;
const EDGE = 18;

export function Shadow({ cx = 160, rx = 104 }: { cx?: number; rx?: number }) {
  return <ellipse className="no-edge" cx={cx} cy="204" rx={rx} ry="8" fill={NYU.ink} opacity="0.08" />;
}

export function Star({ x, y, r = 12 }: { x: number; y: number; r?: number }) {
  const k = r * 0.2;
  return (
    <path
      d={`M${x} ${y - r} Q${x + k} ${y - k} ${x + r} ${y} Q${x + k} ${y + k} ${x} ${y + r} Q${x - k} ${y + k} ${x - r} ${y} Q${x - k} ${y - k} ${x} ${y - r}Z`}
      fill={NYU.star}
      stroke={NYU.ink}
      strokeWidth={r > 10 ? 4 : 3}
    />
  );
}

export function Heart({ x, y, size = 1, fill = NYU.body }: { x: number; y: number; size?: number; fill?: string }) {
  return (
    <path
      transform={`translate(${x} ${y}) scale(${size})`}
      d="M0 13 C-15 3 -18 -4 -17 -8 C-16 -15 -7 -16 -3 -11 L0 -8 L3 -11 C7 -16 16 -15 17 -8 C18 -4 15 3 0 13Z"
      fill={fill}
      stroke={NYU.ink}
      strokeWidth={4 / size}
    />
  );
}

export function Letter({ x, y, rotate = 0, seal = false }: { x: number; y: number; rotate?: number; seal?: boolean }) {
  return (
    <g transform={`translate(${x} ${y}) rotate(${rotate})`}>
      <rect x="-24" y="-16" width="48" height="32" rx="6" fill={NYU.paper} {...S} strokeWidth={5} />
      <path d="M-18 -10 L0 3 L18 -10" fill="none" stroke={NYU.ink} strokeWidth={5} />
      {seal && <Heart x={0} y={4} size={0.42} />}
    </g>
  );
}

function Drop({ x, y }: { x: number; y: number }) {
  return <path d={`M${x} ${y} q-6 9 0 13 q6 -4 0 -13Z`} fill={NYU.tear} stroke={NYU.ink} strokeWidth={3} />;
}

/** Inbox zero: Nyu naps on a cushion next to a cup of tea. */
function Inbox() {
  return (
    <>
      <Shadow />
      <Sticker edge={EDGE}>
        <path d="M52 178 Q52 148 146 146 Q240 148 240 178 Q240 202 146 202 Q52 202 52 178Z" fill={NYU.lilac} {...S} />
        <path d="M256 158 H292 V176 Q292 196 274 196 Q256 196 256 176Z" fill={NYU.paper} {...S} />
        <path d="M292 164 Q306 164 306 173 Q306 183 292 182" fill="none" {...S} />
      </Sticker>
      <g fill="none" stroke={NYU.ink} strokeWidth={4} opacity="0.45" className="nyu-steam">
        <path d="M268 146 q-6 -8 0 -16 q6 -8 0 -16" />
        <path d="M281 146 q-6 -8 0 -16 q6 -8 0 -16" />
      </g>
      <Nyu mood="sleepy" x={146} y={116} scale={0.4} tilt={-4} />
      <Sticker edge={12}>
        <g fill="none" stroke={NYU.ink} className="nyu-zzz">
          <path d="M214 48 h12 l-12 12 h12" strokeWidth={4} />
          <path d="M234 20 h17 l-17 17 h17" strokeWidth={5} />
        </g>
      </Sticker>
    </>
  );
}

/** Search without results: Nyu looks through a magnifier. */
function Search() {
  return (
    <>
      <Shadow cx={150} />
      <Sticker edge={EDGE}>
        <path d="M214 118 L196 142" stroke={NYU.ink} strokeWidth={14} />
        <circle cx="236" cy="90" r="30" fill={NYU.sky} {...S} />
        <path d="M220 78 q7 -9 18 -9" fill="none" stroke={NYU.paper} strokeWidth={5} />
        <path d="M44 42 q0 -15 15 -15 q15 0 15 13 q0 9 -12 13 v7" fill="none" stroke={NYU.ink} strokeWidth={7} />
        <circle cx="62" cy="75" r="4.5" fill={NYU.ink} />
      </Sticker>
      <Nyu mood="puzzled" x={130} y={122} scale={0.4} tilt={-6} front={<Paw x={420} y={302} />} />
      <Sticker edge={EDGE}>
        <Letter x={52} y={186} rotate={-14} />
        <Letter x={268} y={186} rotate={10} />
      </Sticker>
    </>
  );
}

/** An empty folder: Nyu sits happily in an empty box. */
function EmptyFolder() {
  return (
    <>
      <Shadow />
      <Sticker edge={EDGE}>
        <path d="M86 120 H234 L228 134 H92Z" fill="#DDA56A" {...S} />
      </Sticker>
      <Nyu mood="uwu" x={160} y={110} scale={0.38} />
      <Sticker edge={EDGE}>
        <path d="M84 128 L48 106 L64 90 L112 128Z" fill={NYU.kraftLight} {...S} />
        <path d="M236 128 L272 106 L256 90 L208 128Z" fill={NYU.kraftLight} {...S} />
        <path d="M78 128 H242 L230 200 H90Z" fill={NYU.kraft} {...S} />
        <Heart x={160} y={164} size={0.8} />
      </Sticker>
      <Sticker edge={12}>
        <path d="M262 58 q16 -24 40 -14 q-14 24 -40 14Z" fill={NYU.mint} stroke={NYU.ink} strokeWidth={4} />
        <path d="M268 55 q14 -6 26 -9" fill="none" stroke={NYU.ink} strokeWidth={3} />
      </Sticker>
      <path
        d="M22 70 q22 -10 44 0 q14 6 22 -3 M34 90 q16 -7 32 0"
        fill="none"
        stroke={NYU.ink}
        strokeWidth={4}
        opacity="0.35"
      />
    </>
  );
}

/** Nothing selected: Nyu waves towards the list. */
function Pick() {
  return (
    <>
      <Shadow cx={180} />
      <Sticker edge={EDGE}>
        <Letter x={62} y={84} rotate={-12} seal />
      </Sticker>
      <path d="M98 98 h18 M104 112 h12" stroke={NYU.ink} strokeWidth={4} opacity="0.35" />
      <Nyu mood="happy" x={188} y={122} scale={0.4} tilt={5} front={<Paw x={84} y={236} className="nyu-wave" />} />
      <Sticker edge={12}>
        <Star x={286} y={48} r={12} />
        <Star x={278} y={184} r={8} />
      </Sticker>
    </>
  );
}

/** No preview for a file: Nyu holds up a page with a question mark. */
function NoPreview() {
  return (
    <>
      <Shadow cx={150} />
      <Nyu mood="puzzled" x={112} y={126} scale={0.4} tilt={-8} />
      <Sticker edge={EDGE}>
        <g transform="rotate(8 226 118)">
          <path d="M190 58 H240 L262 80 V176 H190Z" fill={NYU.paper} {...S} />
          <path d="M240 58 V80 H262" fill={NYU.flap} {...S} />
          <path d="M212 106 q0 -15 15 -15 q15 0 15 13 q0 10 -13 14 v8" fill="none" stroke={NYU.body} strokeWidth={9} />
          <circle cx="229" cy="145" r="5.5" fill={NYU.body} />
        </g>
        <ellipse cx="190" cy="136" rx="13" ry="11" fill={NYU.body} {...S} />
      </Sticker>
    </>
  );
}

/** No addons yet: Nyu gets starry eyes over a puzzle piece. */
function Addons() {
  return (
    <>
      <Shadow cx={150} />
      <Nyu mood="sparkle" x={128} y={122} scale={0.4} tilt={-5} />
      <Sticker edge={EDGE}>
        <g transform="rotate(14 244 96)">
          <path
            d="M216 68 H234 A10 10 0 0 1 254 68 H272 V86 A10 10 0 0 1 272 106 V124 H216 V106 A10 10 0 0 0 216 86 Z"
            fill={NYU.mint}
            {...S}
          />
        </g>
      </Sticker>
      <Sticker edge={12}>
        <Star x={292} y={40} r={13} />
        <Star x={222} y={170} r={9} />
        <Star x={40} y={56} r={10} />
      </Sticker>
    </>
  );
}

/** First start: Nyu says hello. */
function Welcome() {
  return (
    <>
      <Shadow />
      <path d="M246 44 q12 9 10 25 M262 32 q16 13 14 35" fill="none" stroke={NYU.ink} strokeWidth={4} opacity="0.4" />
      <Nyu mood="happy" x={156} y={124} scale={0.42} tilt={-6} front={<Paw x={440} y={196} className="nyu-wave" />} />
      <Sticker edge={12}>
        <Heart x={56} y={62} size={0.95} />
        <Star x={282} y={150} r={11} />
        <Star x={40} y={150} r={8} />
      </Sticker>
    </>
  );
}

const CONFETTI: [x: number, y: number, rotate: number, fill: string][] = [
  [42, 40, -20, NYU.body],
  [78, 16, 30, NYU.star],
  [118, 34, 70, NYU.mint],
  [210, 22, -40, NYU.lilac],
  [250, 44, 15, NYU.body],
  [284, 20, 60, NYU.sky],
  [30, 108, 45, NYU.sky],
  [292, 104, -30, NYU.star],
  [58, 164, 20, NYU.lilac],
  [268, 170, -60, NYU.mint],
];

/** Setup done: Nyu cheers with both paws up. */
function Done() {
  return (
    <>
      <Shadow />
      <Sticker edge={10}>
        {CONFETTI.map(([x, y, rotate, fill]) => (
          <rect
            key={`${x}-${y}`}
            x={x - 7}
            y={y - 4}
            width="14"
            height="8"
            rx="2"
            transform={`rotate(${rotate} ${x} ${y})`}
            fill={fill}
            stroke={NYU.ink}
            strokeWidth={3}
          />
        ))}
      </Sticker>
      <Nyu
        mood="cheer"
        x={160}
        y={128}
        scale={0.42}
        front={
          <>
            <Paw x={82} y={184} />
            <Paw x={430} y={184} />
          </>
        }
      />
      <Sticker edge={12}>
        <Star x={160} y={24} r={11} />
      </Sticker>
    </>
  );
}

/** A message failed to load: Nyu got tangled in a cable. */
function LoadError() {
  const cable = "M34 190 C66 160 90 208 124 176 S206 118 210 160 S140 180 182 198 S246 194 256 176";
  return (
    <>
      <Shadow cx={150} />
      <Nyu mood="sad" x={146} y={114} scale={0.4} tilt={6} />
      <Sticker edge={16}>
        <path d={cable} fill="none" stroke={NYU.ink} strokeWidth={12} />
      </Sticker>
      <path d={cable} fill="none" stroke={NYU.violet} strokeWidth={5} />
      <Sticker edge={EDGE}>
        <g transform="rotate(-28 270 168)">
          <path d="M284 162 h12 M284 174 h12" stroke={NYU.ink} strokeWidth={5} />
          <rect x="254" y="156" width="32" height="24" rx="6" fill={NYU.lilac} {...S} />
        </g>
      </Sticker>
    </>
  );
}

/** Offline with nothing saved: Nyu waits under an umbrella. */
function Offline() {
  return (
    <>
      <Shadow cx={170} />
      <Sticker edge={12}>
        <path
          d="M40 48 A14 14 0 0 1 50 22 A21 21 0 0 1 90 16 A17 17 0 0 1 118 36 A11 11 0 0 1 114 48 Z"
          fill={NYU.cloud}
          stroke={NYU.ink}
          strokeWidth={5}
        />
        <Drop x={54} y={62} />
        <Drop x={80} y={78} />
        <Drop x={104} y={60} />
        <Drop x={66} y={98} />
      </Sticker>
      <Nyu mood="puzzled" x={172} y={152} scale={0.36} tilt={-4} />
      <Sticker edge={EDGE}>
        <g transform="translate(222 154) rotate(-16)">
          <path d="M0 0 V-96" stroke={NYU.ink} strokeWidth={6} />
          <path
            d="M-70 -76 Q0 -142 70 -76 Q52.5 -87 35 -76 Q17.5 -87 0 -76 Q-17.5 -87 -35 -76 Q-52.5 -87 -70 -76Z"
            fill={NYU.star}
            {...S}
          />
          <path d="M0 -106 v-6" stroke={NYU.ink} strokeWidth={7} />
        </g>
        <ellipse cx="220" cy="152" rx="13" ry="11" fill={NYU.body} {...S} />
      </Sticker>
    </>
  );
}

/** No account yet: Nyu peeks at an empty mailbox. */
function NoAccount() {
  return (
    <>
      <Shadow cx={170} rx={120} />
      <Sticker edge={EDGE}>
        <rect x="222" y="120" width="16" height="84" rx="4" fill={NYU.lilac} {...S} />
        <path d="M184 130 V92 A46 46 0 0 1 276 92 V130 Z" fill={NYU.sky} {...S} />
        <path d="M200 130 V96 A30 30 0 0 1 260 96 V130 Z" fill={NYU.ink} />
        <path d="M276 100 H298" stroke={NYU.ink} strokeWidth={6} />
        <path d="M298 100 V126 H284 V112 H298" fill={NYU.body} {...S} strokeWidth={5} />
      </Sticker>
      <Nyu mood="puzzled" x={104} y={136} scale={0.38} tilt={10} front={<Paw x={430} y={250} />} />
    </>
  );
}

const SCENES = {
  inbox: Inbox,
  search: Search,
  emptyFolder: EmptyFolder,
  pick: Pick,
  noPreview: NoPreview,
  addons: Addons,
  welcome: Welcome,
  done: Done,
  loadError: LoadError,
  offline: Offline,
  noAccount: NoAccount,
} satisfies Record<string, () => ReactNode>;

export type SceneName = keyof typeof SCENES;

export const SCENE_NAMES = Object.keys(SCENES) as SceneName[];

/** A small illustration of Nyu for empty and error states. Decorative only. */
export function NyuScene({ name, className }: { name: SceneName; className?: string }) {
  const Scene = SCENES[name];
  return (
    <svg
      viewBox="-10 -10 340 230"
      className={clsx("nyu-host nyu-blink overflow-visible", className)}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <Scene />
    </svg>
  );
}
