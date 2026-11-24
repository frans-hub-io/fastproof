test('runs in another worker', () => {
  console.log('hello from the second file');
  expect(process.env.FASTPROOF_JEST_STAGE).toBeTruthy();
});
