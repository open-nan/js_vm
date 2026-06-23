// @expect 18
// @seeds 8

let total = 0;
let i = 0;

do {
  total = total + i;
  i++;
} while (i < 4);

let label = 0;
switch (total) {
  case 5:
    label = 1;
    break;
  case 6:
    label = 2;
    break;
  default:
    label = 3;
}

(label = label + 10, label + total);
