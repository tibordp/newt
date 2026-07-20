import { PreferencesState } from "../../../lib/preferences";

export type SettingDef = {
  key: string;
  title: string;
  description: string;
  category: string;
  categoryTitle: string;
  type: "boolean" | "string" | "number" | "enum" | "custom";
  enumValues?: string[];
  enumLabels?: Record<string, string>;
  customWidget?: string;
  value: any;
  modified: boolean;
};

/// Extract enum values (and optional per-value labels) from a resolved schema
/// node. schemars emits two shapes for unit enums: the compact
/// `{ type: "string", enum: [...] }`, and — when variants carry a `title`
/// (or doc comment) — a `oneOf` of `{ title, enum: [oneValue] }`. The `title`
/// becomes the dropdown label (e.g. `.tar.zst` for the archive format).
function extractEnum(propSchema: any): {
  values?: string[];
  labels?: Record<string, string>;
} {
  if (propSchema.type === "string" && Array.isArray(propSchema.enum)) {
    return { values: propSchema.enum };
  }
  if (Array.isArray(propSchema.oneOf)) {
    const values: string[] = [];
    const labels: Record<string, string> = {};
    for (const variant of propSchema.oneOf) {
      const v = variant?.enum?.[0] ?? variant?.const;
      if (typeof v !== "string") return {};
      values.push(v);
      if (typeof variant.title === "string") labels[v] = variant.title;
    }
    return {
      values,
      labels: Object.keys(labels).length ? labels : undefined,
    };
  }
  return {};
}

export function resolveRef(schema: any, refPath: string): any {
  // Resolve "#/definitions/Foo" style $ref pointers
  const parts = refPath.replace(/^#\//, "").split("/");
  let node = schema;
  for (const part of parts) {
    node = node?.[part];
  }
  return node;
}

export function resolveSchema(root: any, node: any): any {
  if (!node) return node;
  // Direct $ref
  if (node.$ref) return resolveRef(root, node.$ref);
  // allOf with a single $ref (schemars pattern)
  if (node.allOf?.length === 1 && node.allOf[0].$ref) {
    return resolveRef(root, node.allOf[0].$ref);
  }
  return node;
}

export function extractSettings(preferences: PreferencesState): SettingDef[] {
  const settings: SettingDef[] = [];
  // schema is `JsonValue` from the bindings (the raw JSON Schema as
  // serde_json::Value); we walk it dynamically, so the `any` cast at the
  // boundary is intentional.
  const schema = preferences.schema as any;
  const values = preferences.settings;

  if (!schema?.properties) return settings;

  // Walk the schema properties (top-level = categories)
  for (const [category, rawCatSchema] of Object.entries(schema.properties) as [
    string,
    any,
  ][]) {
    const catSchema = resolveSchema(schema, rawCatSchema);
    if (catSchema?.type !== "object" || !catSchema.properties) continue;
    const categoryTitle =
      rawCatSchema.title || catSchema.title || category.replace(/_/g, " ");

    for (const [prop, rawPropSchema] of Object.entries(
      catSchema.properties,
    ) as [string, any][]) {
      const propSchema = resolveSchema(schema, rawPropSchema);
      const key = `${category}.${prop}`;
      const title =
        rawPropSchema.title || propSchema.title || prop.replace(/_/g, " ");
      const description =
        rawPropSchema.description || propSchema.description || "";

      const { values: enumValues, labels: enumLabels } =
        extractEnum(propSchema);

      // Custom widget registry: keys that get special UI instead of generic controls
      const customWidgets: Record<string, string> = {
        "appearance.columns": "columns",
        "behavior.default_sort": "default_sort",
      };
      const customWidget = customWidgets[key];

      const type: SettingDef["type"] = customWidget
        ? "custom"
        : enumValues
          ? "enum"
          : propSchema.type === "boolean"
            ? "boolean"
            : propSchema.type === "integer" || propSchema.type === "number"
              ? "number"
              : "string";

      const value = (values as any)?.[category]?.[prop];

      settings.push({
        key,
        title,
        description,
        category,
        categoryTitle,
        type,
        enumValues,
        enumLabels,
        customWidget,
        value,
        modified: preferences.modified_keys.includes(key),
      });
    }
  }

  return settings;
}
