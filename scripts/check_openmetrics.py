"""Parse an OpenMetrics exposition strictly, failing on any error.

Usage: python3 scripts/check_openmetrics.py FILE
Needs the `prometheus-client` package, whose OpenMetrics parser rejects
anything the OpenMetrics 1.0 specification doesn't allow.
"""

import sys

from prometheus_client.openmetrics.parser import text_string_to_metric_families


def main(path: str) -> None:
    text = open(path, encoding="utf-8").read()
    families = list(text_string_to_metric_families(text))
    samples = sum(len(f.samples) for f in families)
    exemplars = sum(1 for f in families for s in f.samples if s.exemplar)
    if not families or not exemplars:
        sys.exit(f"{path}: expected metrics with exemplars")
    print(f"{path}: valid OpenMetrics, {len(families)} families, {samples} samples, {exemplars} exemplars")


if __name__ == "__main__":
    main(sys.argv[1])
