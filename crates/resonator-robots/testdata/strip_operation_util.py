"""Prints OperationUtil.java without the methods that need the wave server.

Used to build ApiGoldenGenerator.java, see there. The methods submitDeltas
and buildSupplement, and the imports only they need, are removed.
"""
import re
import sys

source = open(sys.argv[1]).read()
for method in ("submitDeltas", "buildSupplement"):
    start = source.index("  public static", source.rindex("/**", 0, source.index(" " + method + "(")))
    start = source.rindex("/**", 0, start)
    depth = 0
    i = source.index("{", source.index(" " + method + "("))
    while True:
        if source[i] == "{":
            depth += 1
        elif source[i] == "}":
            depth -= 1
            if depth == 0:
                break
        i += 1
    source = source[:start] + source[i + 1:]
dropped = (
    "waveserver", "CoreWaveletOperationSerializer", "federation", "supplement",
    "RobotWaveletData", "OperationResults", "WaveletDelta;", "ConversationView",
    "java.util.Map.Entry", "IdUtil", "InvalidIdException", "WaveletName", "WaveId;",
    "WaveletId;", "ApiIdSerializer",
)
lines = [
    line for line in source.split("\n")
    if not (line.startswith("import ") and any(word in line for word in dropped))
]
print("\n".join(lines))
