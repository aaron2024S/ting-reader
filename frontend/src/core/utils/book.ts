const unknownContributors = new Set([
  'unknown', 'unknow', 'unknown author', 'unknown narrator', 'unknown artist',
  '未知', '未知作者', '未知演播', '未知主播', '未知演播者', '未知演播人',
]);

export function bookCredits(author?: string | null, narrator?: string | null): string {
  return [author, narrator]
    .map(value => value?.trim() || '')
    .filter(value => value && !unknownContributors.has(value.toLowerCase()))
    .join(' · ');
}

export function bookPrimaryCredit(author?: string | null, narrator?: string | null): string {
  return bookCredits(author) || bookCredits(undefined, narrator);
}
