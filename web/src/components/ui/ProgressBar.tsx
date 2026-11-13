export function ProgressBar({ done, total, label }: { done: number; total: number; label: string }) {
  const share = total > 0 ? Math.min(1, done / total) : 0;
  return (
    <div
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={Math.round(share * 100)}
      className="h-2.5 overflow-hidden rounded-full bg-canvas"
    >
      <div className="h-full rounded-full bg-pink transition-[width]" style={{ width: `${share * 100}%` }} />
    </div>
  );
}
