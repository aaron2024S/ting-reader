import assert from 'node:assert/strict';
import { test } from 'node:test';
import { bookCredits, bookPrimaryCredit } from '../src/core/utils/book.ts';

test('book credits omit missing and legacy unknown placeholders in either field', () => {
  for (const unknown of [undefined, null, '', '  ', 'Unknown', ' unknow ', 'UNKNOWN AUTHOR',
    'Unknown Narrator', 'Unknown Artist', '未知', '未知作者', '未知演播', '未知主播', '未知演播者', '未知演播人']) {
    assert.equal(bookCredits(unknown, unknown), '');
    assert.equal(bookCredits(unknown, ' 演播者 '), '演播者');
    assert.equal(bookCredits(' 作者 ', unknown), '作者');
    assert.equal(bookPrimaryCredit(unknown, unknown), '');
    assert.equal(bookPrimaryCredit(unknown, ' 演播者 '), '演播者');
    assert.equal(bookPrimaryCredit(' 作者 ', unknown), '作者');
  }
  assert.equal(bookCredits('作者', '演播者'), '作者 · 演播者');
  assert.equal(bookCredits('未知君', 'Unknown Stories'), '未知君 · Unknown Stories');
  assert.equal(bookPrimaryCredit(' 作者 ', ' 演播者 '), '作者');
  assert.equal(bookPrimaryCredit('未知君', 'Unknown Stories'), '未知君');
});
