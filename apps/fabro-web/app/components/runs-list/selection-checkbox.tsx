export function SelectionCheckbox({
  checked,
  indeterminate = false,
  disabled = false,
  onChange,
  ariaLabel,
}: {
  checked:        boolean;
  indeterminate?: boolean;
  disabled?:      boolean;
  onChange:       () => void;
  ariaLabel:      string;
}) {
  return (
    <input
      ref={(input) => {
        if (input) input.indeterminate = indeterminate;
      }}
      type="checkbox"
      aria-label={ariaLabel}
      checked={checked}
      disabled={disabled}
      onChange={onChange}
      onClick={(e) => e.stopPropagation()}
      className="size-4 cursor-pointer rounded border border-line-strong bg-panel/80 text-focus outline-none focus:ring-1 focus:ring-focus disabled:cursor-default disabled:opacity-40"
    />
  );
}
