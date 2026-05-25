export function plural(n: number, singular: string, pluralForm: string) {
  return n === 1 ? singular : pluralForm;
}
