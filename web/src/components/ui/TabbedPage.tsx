import type { ReactNode } from "react";
import { PageHeader } from "./Card";
import { Tabs, type Tab } from "./Tabs";

/**
 * A page that gathers several things behind tabs: one heading, the tabs, and what the current tab
 * shows. The intro belongs to the tab, so it says what this tab is for rather than the whole page.
 */
export function TabbedPage<T extends string>({
  title,
  intro,
  label,
  value,
  tabs,
  children,
}: {
  title: ReactNode;
  intro?: ReactNode;
  /** What the row of tabs is called for screen readers. */
  label: string;
  value: T;
  tabs: Tab<T>[];
  children: ReactNode;
}) {
  return (
    <div className="flex flex-col gap-5">
      <PageHeader title={title} intro={intro} />
      <Tabs<T> label={label} value={value} tabs={tabs} />
      {children}
    </div>
  );
}
