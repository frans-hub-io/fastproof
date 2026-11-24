describe('cart', () => {
  it('passes and logs [fastproof-id:CASE-1]', () => {
    cy.log('adding an item');
    expect(1 + 1).to.eq(2);
  });

  it('flaky: passes on third try', () => {
    cy.task('attempt').then((n) => {
      expect(n).to.be.at.least(3);
    });
  });

  it('fails every time', () => {
    expect('a').to.eq('b');
  });

  it.skip('skipped', () => {});
});
