function Test262Error(message) {
  this.name = 'Test262Error';
  this.message = message || '';
}

Test262Error.prototype = Object.create(Error.prototype);
Test262Error.prototype.constructor = Test262Error;

function $ERROR(message) {
  throw new Test262Error(message);
}

function sameValue(actual, expected) {
  if (actual === expected) {
    return actual !== 0 || 1 / actual === 1 / expected;
  }
  return actual !== actual && expected !== expected;
}

var assert = {
  sameValue(actual, expected, message) {
    if (!sameValue(actual, expected)) {
      $ERROR(message || 'Expected SameValue equality');
    }
  },
  notSameValue(actual, unexpected, message) {
    if (sameValue(actual, unexpected)) {
      $ERROR(message || 'Expected SameValue inequality');
    }
  },
  throws(expectedErrorConstructor, fn, message) {
    try {
      fn();
    } catch (error) {
      if (error instanceof expectedErrorConstructor) return;
      $ERROR(message || 'Expected a different error constructor');
    }
    $ERROR(message || 'Expected function to throw');
  }
};
