import { useCategories } from "../lib/categories";

/** Neutral zinc for unknown/historical ids not in the registry. */
const FALLBACK_COLOR = "#a1a1aa";

export default function CategoryBadge({ category }: { category: string }) {
  const { categories } = useCategories();
  const record =
    categories.find((c) => c.category_id === category) ?? null;
  const color = record?.color ?? FALLBACK_COLOR;
  return (
    <span
      className="category-badge inline-flex items-center whitespace-nowrap rounded-full border px-2 py-0.5 text-xs font-medium"
      style={{
        ["--cat" as string]: color,
        backgroundColor: `${color}22`,
        borderColor: `${color}44`,
      }}
      title={record?.description || undefined}
    >
      {record?.display_name ?? category}
    </span>
  );
}
