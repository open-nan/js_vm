// @expect 9
// @seeds 6

const input = ({ value: 4 } as { value: number });
const output = (input.value as number) + 5;

output;
