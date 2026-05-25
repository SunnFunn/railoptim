interface RailwayFilterProps {
  label: string;
  options: string[];
  selected: Set<string>;
  onChange: (next: Set<string>) => void;
}

export function RailwayFilter({
  label,
  options,
  selected,
  onChange,
}: RailwayFilterProps) {
  return (
    <div className="filter-block">
      <div className="filter-label">{label}</div>
      <select
        multiple
        size={Math.min(8, Math.max(3, options.length))}
        className="filter-select"
        value={[...selected]}
        onChange={(e) => {
          const next = new Set<string>();
          for (const opt of e.target.selectedOptions) {
            next.add(opt.value);
          }
          onChange(next);
        }}
      >
        {options.map((rw) => (
          <option key={rw} value={rw}>
            {rw}
          </option>
        ))}
      </select>
      <div className="filter-hint">
        {selected.size === 0
          ? "Все дороги (Ctrl/Cmd+клик для нескольких)"
          : `Выбрано: ${selected.size}`}
      </div>
      {options.length > 0 && (
        <button type="button" className="btn-link" onClick={() => onChange(new Set())}>
          Сбросить
        </button>
      )}
    </div>
  );
}
