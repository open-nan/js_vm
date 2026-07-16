var $DONE = function(error) {
  if (error) {
    print('Test262:AsyncTestFailure: ' + error);
  } else {
    print('Test262:AsyncTestComplete');
  }
};
