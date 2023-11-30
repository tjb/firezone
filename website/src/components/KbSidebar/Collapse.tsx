import { HiChevronRight, HiChevronDown } from "react-icons/hi2";
import { useState } from "react";

export default function Collapse({
  children,
  label,
  expanded,
}: {
  children: React.ReactNode;
  label: string;
  expanded?: boolean;
}) {
  const ctl = label.toLowerCase().replace(" ", "-") + "-dropdown";
  const indent = "ml-3";
  const [expandedState, setExpandedState] = useState(false);

  return (
    <>
      <button
        type="button"
        className={
          " flex items-center w-full transition duration-75 rounded group hover:bg-neutral-100 "
        }
        aria-controls={ctl}
        data-collapse-toggle={ctl}
        onClick={() => setExpandedState(!expandedState)}
      >
        <span
          className="ml-3 flex-1 text-left whitespace-nowrap font-semibold"
          sidebar-toggle-item="true"
        >
          {label}
        </span>
        {expandedState ? (
          <HiChevronDown sidebar-toggle-item="true" className="w-5 h-5" />
        ) : (
          <HiChevronRight sidebar-toggle-item="true" className="w-5 h-5" />
        )}
      </button>
      <ul
        id={ctl}
        className={[
          expandedState ? "" : "hidden",
          "ml-3 py-1 space-y-0.5",
        ].join(" ")}
      >
        {children}
      </ul>
    </>
  );
}