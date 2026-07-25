export type BookFilterSchema = {
  hasQualityFlags: boolean;
};

export function validBookWhere(schema: BookFilterSchema, alias: string): string {
  const prefix = alias ? `${alias}.` : '';
  const predicates = [
    `${prefix}bid_price IS NOT NULL`,
    `${prefix}ask_price IS NOT NULL`,
    `${prefix}bid_price > 0`,
    `${prefix}ask_price > 0`,
    `${prefix}bid_price <= ${prefix}ask_price`,
    `(${prefix}bid_size IS NULL OR ${prefix}bid_size > 0)`,
    `(${prefix}ask_size IS NULL OR ${prefix}ask_size > 0)`
  ];
  if (schema.hasQualityFlags) {
    predicates.push(
      `${prefix}quality_gap = false`,
      `${prefix}quality_stale = false`,
      `${prefix}quality_inconsistent = false`
    );
  }
  return predicates.join('\n      AND ');
}
