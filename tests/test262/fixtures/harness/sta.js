if (typeof $DONOTEVALUATE === 'undefined') {
  var $DONOTEVALUATE = function() {
    throw new SyntaxError('$DONOTEVALUATE was evaluated');
  };
}

if (typeof $DONE === 'undefined') {
  var $DONE = function(error) {
    if (error) {
      print('Test262:AsyncTestFailure: ' + error);
    } else {
      print('Test262:AsyncTestComplete');
    }
  };
}
