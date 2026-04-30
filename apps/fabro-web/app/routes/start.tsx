import { useState, useRef, useEffect } from "react";
import {
  Listbox,
  ListboxButton,
  ListboxOption,
  ListboxOptions,
} from "@headlessui/react";
import { ArrowUpIcon } from "@heroicons/react/24/solid";
import {
  ChevronUpDownIcon,
  FolderIcon,
} from "@heroicons/react/16/solid";
import {
  BugAntIcon,
  CodeBracketIcon,
  MagnifyingGlassIcon,
  XMarkIcon,
} from "@heroicons/react/24/outline";
import { useSystemInfo } from "../lib/queries";

export const handle = { hideHeader: true, wide: true };

export function meta({}: any) {
  return [{ title: "Start — Fabro" }];
}

const projects = [
  { id: "fabro-web", name: "fabro-web" },
  { id: "fabro-workflows", name: "fabro-workflows" },
  { id: "fabro-cli", name: "fabro-cli" },
];

const branches = [
  { id: "main", name: "main" },
  { id: "develop", name: "develop" },
  { id: "feature/start-page", name: "feature/start-page" },
];

function BranchIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" className={className}>
      <path d="M9.5 3.25a2.25 2.25 0 1 1 3 2.122V6A2.5 2.5 0 0 1 10 8.5H6a1 1 0 0 0-1 1v1.128a2.251 2.251 0 1 1-1.5 0V5.372a2.25 2.25 0 1 1 1.5 0v1.836A2.5 2.5 0 0 1 6 7h4a1 1 0 0 0 1-1v-.628A2.25 2.25 0 0 1 9.5 3.25Zm-6 0a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Zm8.25-.75a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5ZM4.25 12a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Z" />
    </svg>
  );
}

export default function Start() {
  const systemInfo = useSystemInfo();
  const features = systemInfo.data?.features ?? {
    session_sandboxes: false,
    retros: false,
  };
  const [prompt, setPrompt] = useState("");
  const [project, setProject] = useState(projects[0]);
  const [branch, setBranch] = useState(branches[0]);
  const [openCategory, setOpenCategory] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    textareaRef.current?.focus();
  }, []);

  function autoResize() {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 280) + "px";
  }

  function handleKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      if (prompt.trim()) handleSubmit();
    }
  }

  function handleSubmit() {
    if (!prompt.trim()) return;
    // TODO: wire up submission
  }

  return (
    <div className="flex -mx-4 sm:-mx-6 lg:-mx-8 -my-6">
      <div className="flex-1 flex flex-col items-center pt-[12vh] px-4">
        <div className="w-full max-w-2xl">
          <h1 className="flex items-center justify-center gap-3 text-[2rem] font-medium tracking-tight text-fg-2 text-center mb-8">
            <img src="/images/logo.svg" alt="" className="size-9" />
            What do you want to build?
          </h1>

          <div className="relative group">
            <div className="absolute -inset-px rounded-xl bg-gradient-to-b from-teal-500/30 to-mint/20 opacity-0 blur-sm transition-opacity duration-300 group-focus-within:opacity-100" />

            <div className="relative rounded-xl bg-panel border border-line-strong group-focus-within:border-focus transition-colors duration-300">
              <textarea
                ref={textareaRef}
                value={prompt}
                onChange={(e) => {
                  setPrompt(e.target.value);
                  autoResize();
                }}
                onKeyDown={handleKeyDown}
                placeholder="Describe a workflow, pipeline, or automation..."
                rows={3}
                className="w-full resize-none bg-transparent px-5 pt-4 pb-14 text-[15px] leading-relaxed text-fg-2 placeholder:text-fg-muted focus:outline-none"
              />

              <div className="absolute bottom-3 inset-x-3 flex items-center justify-between">
                {features.session_sandboxes && (
                  <div className="flex items-center gap-1.5">
                    <Picker
                      value={project}
                      onChange={setProject}
                      options={projects}
                      icon={<FolderIcon className="size-3.5 text-fg-muted" />}
                    />
                    <Picker
                      value={branch}
                      onChange={setBranch}
                      options={branches}
                      icon={<BranchIcon className="size-3.5 text-fg-muted" />}
                    />
                  </div>
                )}

                <div className="ml-auto flex items-center gap-3">
                  <span className="text-xs text-fg-muted select-none">
                    <kbd className="font-mono">Enter</kbd> to submit
                  </span>
                  <button
                    onClick={handleSubmit}
                    disabled={!prompt.trim()}
                    className="flex items-center justify-center size-8 rounded-lg bg-teal-500 text-on-primary transition-all duration-200 hover:bg-teal-300 disabled:opacity-30 disabled:hover:bg-teal-500 disabled:cursor-default"
                  >
                    <ArrowUpIcon className="size-4" />
                  </button>
                </div>
              </div>
            </div>
          </div>

          <div className="relative mt-5">
            <div className="flex items-center justify-center gap-2">
              {categories.map((cat) => (
                <button
                  key={cat.label}
                  onClick={() => setOpenCategory(openCategory === cat.label ? null : cat.label)}
                  className={`inline-flex items-center gap-2 rounded-full border px-4 py-2 text-sm transition-colors ${
                    openCategory === cat.label
                      ? "border-teal-500/30 bg-teal-500/10 text-teal-300"
                      : "border-line bg-panel/50 text-fg-3 hover:bg-panel hover:border-line-strong"
                  }`}
                >
                  <cat.icon className="size-4" />
                  {cat.label}
                </button>
              ))}
            </div>

            {openCategory && (
              <div className="absolute inset-x-0 top-0 z-10">
                <CategoryPanel
                  category={categories.find((c) => c.label === openCategory)!}
                  onClose={() => setOpenCategory(null)}
                  onSelect={(p) => {
                    setPrompt(p);
                    setOpenCategory(null);
                    textareaRef.current?.focus();
                    setTimeout(() => {
                      const el = textareaRef.current;
                      if (!el) return;
                      el.style.height = "auto";
                      el.style.height = Math.min(el.scrollHeight, 280) + "px";
                    }, 0);
                  }}
                />
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

interface Category {
  label: string;
  icon: React.ComponentType<{ className?: string }>;
  items: { title: string; prompt: string }[];
}

const categories: Category[] = [
  {
    label: "Build",
    icon: CodeBracketIcon,
    items: [
      { title: "Implement a new feature", prompt: "Implement a new feature that adds user authentication with OAuth2, including login, logout, and session management." },
      { title: "Create an API endpoint", prompt: "Create a new REST API endpoint with request validation, error handling, and proper HTTP status codes." },
      { title: "Set up a CI/CD pipeline", prompt: "Set up a CI/CD pipeline with build, test, lint, and deploy stages for the main branch." },
      { title: "Add database migrations", prompt: "Add database migrations to create the new tables and indexes needed for the upcoming feature." },
    ],
  },
  {
    label: "Review",
    icon: MagnifyingGlassIcon,
    items: [
      { title: "Review a pull request", prompt: "Review the latest pull request for bugs, security vulnerabilities, and code style issues. Summarize findings and suggest fixes." },
      { title: "Audit dependencies", prompt: "Audit all project dependencies for known vulnerabilities, outdated versions, and unused packages." },
      { title: "Analyze test coverage", prompt: "Analyze the current test coverage, identify untested code paths, and recommend which areas need tests most." },
      { title: "Check for security issues", prompt: "Scan the codebase for common security vulnerabilities including injection, XSS, and authentication flaws." },
    ],
  },
  {
    label: "Fix",
    icon: BugAntIcon,
    items: [
      { title: "Debug a failing test", prompt: "Debug the failing test suite, identify the root cause of each failure, and apply fixes." },
      { title: "Fix a production bug", prompt: "Investigate and fix the reported production bug, including root cause analysis and a regression test." },
      { title: "Resolve merge conflicts", prompt: "Resolve the merge conflicts in the current branch, preserving the intended changes from both sides." },
      { title: "Fix type errors", prompt: "Fix all TypeScript type errors in the project, ensuring strict type safety without using any type assertions." },
    ],
  },
];

function CategoryPanel({
  category,
  onClose,
  onSelect,
}: {
  category: Category;
  onClose: () => void;
  onSelect: (prompt: string) => void;
}) {
  return (
    <div className="rounded-xl border border-line-strong bg-panel overflow-hidden">
      <div className="flex items-center gap-2 px-4 py-3 border-b border-line">
        <category.icon className="size-4 text-teal-500" />
        <span className="text-sm font-medium text-fg-2">{category.label}</span>
        <button
          onClick={onClose}
          className="ml-auto flex items-center justify-center size-6 rounded-md text-fg-muted hover:text-fg-3 hover:bg-overlay transition-colors"
        >
          <XMarkIcon className="size-4" />
        </button>
      </div>
      <ul>
        {category.items.map((item, i) => (
          <li key={item.title} className={i > 0 ? "border-t border-line" : ""}>
            <button
              onClick={() => onSelect(item.prompt)}
              className="w-full px-4 py-3 text-left text-sm text-fg-3 transition-colors hover:bg-overlay hover:text-fg-2"
            >
              {item.title}
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

function Picker<T extends { id: string; name: string }>({
  value,
  onChange,
  options,
  icon,
}: {
  value: T;
  onChange: (v: T) => void;
  options: T[];
  icon: React.ReactNode;
}) {
  return (
    <Listbox value={value} onChange={onChange}>
      <div className="relative">
        <ListboxButton className="flex items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-xs text-fg-3 bg-page/60 border border-line hover:border-line-strong hover:bg-page/80 transition-colors">
          {icon}
          <span className="max-w-[120px] truncate">{value.name}</span>
          <ChevronUpDownIcon className="size-3.5 text-fg-muted" />
        </ListboxButton>

        <ListboxOptions anchor="top start" className="z-20 w-56 rounded-lg bg-panel border border-line-strong py-1 shadow-xl shadow-black/30 focus:outline-none [--anchor-gap:4px]">
          {options.map((option) => (
            <ListboxOption
              key={option.id}
              value={option}
              className="flex items-center gap-2 px-3 py-1.5 text-xs text-fg-3 data-focus:bg-overlay data-selected:text-teal-300"
            >
              {option.name}
            </ListboxOption>
          ))}
        </ListboxOptions>
      </div>
    </Listbox>
  );
}
