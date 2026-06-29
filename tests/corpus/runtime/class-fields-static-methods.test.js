// @expect 7
// @seeds 8

class Config {
  static version = 2;
  base = 5;

  value() {
    return this.base + Config.version;
  }
}

const config = new Config();
config.value();
