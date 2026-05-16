import { useState } from "react";
import {
  Listbox,
  ListboxButton,
  ListboxOption,
  ListboxOptions,
} from "@headlessui/react";
import {
  ChevronUpDownIcon,
  CheckIcon,
  FolderIcon,
  CpuChipIcon,
} from "@heroicons/react/16/solid";

type Choice = { id: string; label: string };

const PROJECTS: Choice[] = [
  { id: "fabro-web", label: "fabro-web" },
  { id: "fabro-workflows", label: "fabro-workflows" },
  { id: "fabro-cli", label: "fabro-cli" },
];

const BRANCHES: Choice[] = [
  { id: "main", label: "main" },
  { id: "develop", label: "develop" },
  { id: "feature/start-page", label: "feature/start-page" },
];

const MODELS: Choice[] = [
  { id: "claude-opus-4-7", label: "Claude Opus 4.7" },
  { id: "claude-sonnet-4-6", label: "Claude Sonnet 4.6" },
  { id: "gpt-5", label: "GPT-5" },
];

function BranchIcon({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 16 16" fill="currentColor" className={className}>
      <path d="M9.5 3.25a2.25 2.25 0 1 1 3 2.122V6A2.5 2.5 0 0 1 10 8.5H6a1 1 0 0 0-1 1v1.128a2.251 2.251 0 1 1-1.5 0V5.372a2.25 2.25 0 1 1 1.5 0v1.836A2.5 2.5 0 0 1 6 7h4a1 1 0 0 0 1-1v-.628A2.25 2.25 0 0 1 9.5 3.25Zm-6 0a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Zm8.25-.75a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5ZM4.25 12a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Z" />
    </svg>
  );
}

type IconComponent = React.ComponentType<{ className?: string }>;

function Chip({
  options,
  value,
  onChange,
  Icon,
}: {
  options: Choice[];
  value: Choice;
  onChange: (c: Choice) => void;
  Icon: IconComponent;
}) {
  return (
    <Listbox value={value} onChange={onChange}>
      <div className="relative">
        <ListboxButton className="inline-flex items-center gap-1.5 rounded-full bg-overlay px-2.5 py-1 text-xs font-medium text-fg-3 transition-colors hover:bg-overlay-strong hover:text-fg focus:outline-2 focus:outline-offset-1 focus:outline-teal-500">
          <Icon className="size-3.5 text-fg-muted" />
          <span>{value.label}</span>
          <ChevronUpDownIcon className="size-3.5 text-fg-muted" />
        </ListboxButton>
        <ListboxOptions
          anchor={{ to: "bottom start", gap: 6 }}
          className="z-50 w-52 rounded-lg bg-panel p-1 shadow-2xl shadow-black/60 ring-1 ring-line-strong focus:outline-none"
        >
          {options.map((opt) => (
            <ListboxOption
              key={opt.id}
              value={opt}
              className="group flex cursor-pointer items-center gap-2 rounded px-2 py-1.5 text-sm text-fg-2 data-focus:bg-overlay data-focus:text-fg data-selected:text-fg"
            >
              <CheckIcon className="invisible size-4 text-teal-300 group-data-selected:visible" />
              <span>{opt.label}</span>
            </ListboxOption>
          ))}
        </ListboxOptions>
      </div>
    </Listbox>
  );
}

export default function ComposerChips() {
  const [project, setProject] = useState(PROJECTS[0]!);
  const [branch, setBranch] = useState(BRANCHES[0]!);
  const [model, setModel] = useState(MODELS[0]!);
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      <Chip options={PROJECTS} value={project} onChange={setProject} Icon={FolderIcon} />
      <Chip options={BRANCHES} value={branch} onChange={setBranch} Icon={BranchIcon} />
      <Chip options={MODELS} value={model} onChange={setModel} Icon={CpuChipIcon} />
    </div>
  );
}
