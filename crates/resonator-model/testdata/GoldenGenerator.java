// Generates docop_golden.txt from the original Apache Wave Java implementation,
// for the differential test in src/docop/tests/golden.rs.
//
// To regenerate (from the workspace root, with a JDK installed):
//
//   W=reference/apache-wave/wave/src; T=$(mktemp -d)
//   mkdir -p $T/com/google/common/annotations $T/javax/annotation
//   echo 'package com.google.common.annotations; public @interface VisibleForTesting {}' \
//       > $T/com/google/common/annotations/VisibleForTesting.java
//   echo 'package com.google.common.annotations; public @interface GwtCompatible {
//       boolean emulated() default false; boolean serializable() default false; }' \
//       > $T/com/google/common/annotations/GwtCompatible.java
//   echo 'package javax.annotation; public @interface Nullable {}' \
//       > $T/javax/annotation/Nullable.java
//   javac -nowarn -d $T/out -sourcepath $T:$W/main/java:$W/test/java \
//       crates/resonator-model/testdata/GoldenGenerator.java
//   java -cp $T/out GoldenGenerator > crates/resonator-model/testdata/docop_golden.txt
//
// Output format: one record per line, "<tag> <payload>". Operations are
// written with DocOpUtil.toConciseString, documents with DocOpUtil.toXmlString.
//   SEED n   start of a scenario; the random provider is RandomProviderImpl.ofSeed(n)
//   G op     random op, applied to the document (initial mutations)
//   D xml    current document
//   C op     random client op on the current document
//   S op     random server op on the current document
//   TC op    client op transformed against server op
//   TS op    server op transformed against client op
//   CS op    compose(client, transformed server)
//   SC op    compose(server, transformed client)
//   IC op    invert(client)
//   CA op    composition of every op applied in the scenario (DocOpCollector)
// After each C/S group the client op and the transformed server op are
// applied to the document.

import org.waveprotocol.wave.model.document.bootstrap.BootstrapDocument;
import org.waveprotocol.wave.model.document.operation.DocOp;
import org.waveprotocol.wave.model.document.operation.algorithm.Composer;
import org.waveprotocol.wave.model.document.operation.algorithm.DocOpCollector;
import org.waveprotocol.wave.model.document.operation.algorithm.DocOpInverter;
import org.waveprotocol.wave.model.document.operation.algorithm.Transformer;
import org.waveprotocol.wave.model.document.operation.impl.DocOpUtil;
import org.waveprotocol.wave.model.operation.OperationPair;
import org.waveprotocol.wave.model.testing.RandomDocOpGenerator;
import org.waveprotocol.wave.model.testing.RandomProviderImpl;

public class GoldenGenerator {
  private static final int SCENARIOS = 30;
  private static final int INITIAL_MUTATIONS = 3;
  private static final int ITERATIONS = 6;

  private static void line(String tag, DocOp op) {
    System.out.println(tag + " " + DocOpUtil.toConciseString(op));
  }

  private static void doc(BootstrapDocument doc) {
    System.out.println("D " + DocOpUtil.toXmlString(doc.asOperation()));
  }

  public static void main(String[] args) throws Exception {
    RandomDocOpGenerator.Parameters p = new RandomDocOpGenerator.Parameters();
    for (int seed = 0; seed < SCENARIOS; seed++) {
      System.out.println("SEED " + seed);
      RandomDocOpGenerator.RandomProvider r = RandomProviderImpl.ofSeed(seed);
      BootstrapDocument doc = new BootstrapDocument();
      DocOpCollector collector = new DocOpCollector();
      for (int i = 0; i < INITIAL_MUTATIONS; i++) {
        DocOp op = RandomDocOpGenerator.generate(r, p, doc);
        line("G", op);
        doc.consume(op);
        collector.add(op);
      }
      doc(doc);
      for (int i = 0; i < ITERATIONS; i++) {
        DocOp client = RandomDocOpGenerator.generate(r, p, doc);
        DocOp server = RandomDocOpGenerator.generate(r, p, doc);
        line("C", client);
        line("S", server);
        OperationPair<DocOp> pair = Transformer.transform(client, server);
        line("TC", pair.clientOp());
        line("TS", pair.serverOp());
        line("CS", Composer.compose(client, pair.serverOp()));
        line("SC", Composer.compose(server, pair.clientOp()));
        line("IC", DocOpInverter.invert(client));
        doc.consume(client);
        doc.consume(pair.serverOp());
        collector.add(client);
        collector.add(pair.serverOp());
        doc(doc);
      }
      line("CA", collector.composeAll());
    }
  }
}
