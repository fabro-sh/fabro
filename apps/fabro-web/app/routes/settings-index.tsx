import { Fragment } from "react";
import { Link } from "react-router";
import { navSections } from "./settings";

export function meta() {
  return [{ title: "Settings — Fabro" }];
}

export default function SettingsIndex() {
  return (
    <div className="space-y-8">
      {navSections.map((section, sectionIdx) => (
        <Fragment key={section.key}>
          {!section.label && sectionIdx > 0 ? (
            <hr className="border-line" />
          ) : null}
          <section className="space-y-3">
            {section.label ? (
              <h2 className="text-xs font-medium uppercase tracking-wider text-fg-muted">
                {section.label}
              </h2>
            ) : null}
            <ul className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              {section.items.map((item) => (
                <li key={item.href}>
                  <Link
                    to={item.href}
                    className="group flex h-full items-start gap-3 rounded-md border border-line bg-panel/40 p-4 transition-colors hover:border-line-strong hover:bg-overlay"
                  >
                    <item.icon
                      className="size-5 shrink-0 text-fg-3 group-hover:text-fg-2"
                      aria-hidden="true"
                    />
                    <div className="min-w-0">
                      <div className="text-sm font-medium text-fg">{item.name}</div>
                      <div className="mt-0.5 text-xs text-fg-3">{item.description}</div>
                    </div>
                  </Link>
                </li>
              ))}
            </ul>
          </section>
        </Fragment>
      ))}
    </div>
  );
}
