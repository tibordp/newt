import { PreferencesState } from "../../../lib/preferences";

export type SettingDef = {
  key: string;
  title: string;
  description: string;
  category: string;
  categoryTitle: string;
  type: "boolean" | "string" | "number" | "enum" | "custom";
  enumValues?: string[];
  customWidget?: string;
  value: any;
  modified: boolean;
};

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

      // Detect string enums (schemars emits { type: "string", enum: [...] })
      const enumValues: string[] | undefined =
        propSchema.type === "string" && Array.isArray(propSchema.enum)
          ? propSchema.enum
          : undefined;

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
        customWidget,
        value,
        modified: preferences.modified_keys.includes(key),
      });
    }
  }

  return settings;
}
