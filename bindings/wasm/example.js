var libsql = require('./pkg');

var db = new libsql.Database('libsql://penberg.gsoc2.io');

db.all('SELECT 1', function(err, res) {
  if (err) {
    throw err;
  }
  console.log(res[0])
});
