import random
import tinychain as tc
import unittest

from num2words import num2words
from testutils import DEFAULT_PORT, start_host, PersistenceTest

ENDPOINT = "/transact/hypothetical"
SCHEMA = tc.table.Schema(
    [tc.Column("name", tc.String, 512)], [tc.Column("views", tc.UInt)]).create_index("views", ["views"])


class TableTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.host = start_host("test_table")

    def testCreate(self):
        cxt = tc.Context()
        cxt.table = tc.table.Table(SCHEMA)
        cxt.result = tc.After(cxt.table.insert(("name",), (0,)), cxt.table.count())

        count = self.host.post(ENDPOINT, cxt)
        self.assertEqual(count, 1)

    def testDelete(self):
        count = 2
        values = [(v,) for v in range(count)]
        keys = [(num2words(i),) for i in range(count)]

        cxt = tc.Context()
        cxt.table = tc.table.Table(SCHEMA)
        cxt.inserts = [cxt.table.insert(k, v) for k, v in zip(keys, values)]
        cxt.delete = tc.After(cxt.inserts, cxt.table.delete())
        cxt.result = tc.After(cxt.delete, cxt.table)

        result = self.host.post(ENDPOINT, cxt)
        self.assertEqual(result, expected(SCHEMA, []))

    def testInsert(self):
        for x in range(0, 100, 10):
            keys = list(range(x))
            random.shuffle(keys)

            cxt = tc.Context()
            cxt.table = tc.table.Table(SCHEMA)
            cxt.inserts = [
                cxt.table.insert((num2words(i),), (i,))
                for i in keys]

            cxt.result = tc.After(cxt.inserts, cxt.table.count())

            result = self.host.post(ENDPOINT, cxt)
            self.assertEqual(result, x)

    def testLimit(self):
        count = 50
        values = [(v,) for v in range(count)]
        keys = [(num2words(i),) for i in range(count)]

        cxt = tc.Context()
        cxt.table = tc.table.Table(SCHEMA)
        cxt.inserts = [cxt.table.insert(k, v) for k, v in zip(keys, values)]
        cxt.result = tc.After(cxt.inserts, cxt.table.limit(1))

        result = self.host.post(ENDPOINT, cxt)
        first_row = sorted(list(k + v) for k, v in zip(keys, values))[0]
        self.assertEqual(result, expected(SCHEMA, [first_row]))

    def testSelect(self):
        count = 5
        values = [[v] for v in range(count)]
        keys = [[num2words(i)] for i in range(count)]

        cxt = tc.Context()
        cxt.table = tc.table.Table(SCHEMA)
        cxt.inserts = [cxt.table.insert(k, v) for k, v in zip(keys, values)]
        cxt.result = tc.After(cxt.inserts, cxt.table.select(["name"]))

        expected = {
            str(tc.uri(tc.table.Table)): [
                tc.to_json(tc.table.Schema([tc.Column("name", tc.String, 512)])),
                list(sorted(keys))
            ]
        }

        actual = self.host.post(ENDPOINT, cxt)

        self.assertEqual(actual, expected)

    @classmethod
    def tearDownClass(cls):
        cls.host.stop()


class SparseTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.host = start_host("test_sparse_table")

    def testSlice(self):
        schema = tc.table.Schema([
            tc.Column("0", tc.U64),
            tc.Column("1", tc.U64),
            tc.Column("2", tc.U64),
            tc.Column("3", tc.U64),
        ], [
            tc.Column("value", tc.Number),
        ])

        for i in range(4):
            schema.create_index(str(i), [str(i)])

        data = [
            ([0, 0, 1, 0], 1),
            ([0, 1, 2, 0], 2),
            ([1, 0, 0, 0], 3),
            ([1, 0, 1, 0], 3),
        ]

        cxt = tc.Context()
        cxt.table = tc.table.Table(schema)
        cxt.inserts = [cxt.table.insert(coord, [value]) for (coord, value) in data]
        cxt.result = tc.After(cxt.inserts, cxt.table.where({
            "0": slice(2),
            "1": slice(3),
            "2": slice(4),
            "3": slice(1)
        }))

        expect = expected(schema, [coord + [value] for coord, value in data])
        actual = self.host.post(ENDPOINT, cxt)
        self.assertEqual(actual, expect)

    @classmethod
    def tearDownClass(cls):
        cls.host.stop()


class ChainTests(PersistenceTest, unittest.TestCase):
    NAME = "table"
    NUM_HOSTS = 4

    def cluster(self, chain_type):
        class Persistent(tc.Cluster, metaclass=tc.Meta):
            __uri__ = tc.URI(f"http://127.0.0.1:{DEFAULT_PORT}/test/table")

            def _configure(self):
                self.table = tc.chain.Block(tc.table.Table(SCHEMA))

            @tc.delete_method
            def truncate(self):
                return self.table.delete()

        return Persistent

    def execute(self, hosts):
        row1 = ["one", 1]
        row2 = ["two", 2]

        replica_set = set(str(tc.uri(host) + "/test/table") for host in hosts)

        def check_replicas():
            for i in range(len(hosts)):
                replicas = {}
                for replica in hosts[i].get("/test/table/replicas"):
                    replicas.update(replica)

                self.assertEqual(set(replicas.keys()), replica_set, f"host {i}")

        check_replicas()

        self.assertIsNone(hosts[0].put("/test/table/table", ["one"], [1]))

        for host in hosts:
            actual = host.get("/test/table/table", ["one"])
            self.assertEqual(actual, row1)

        hosts[1].stop()
        hosts[2].put("/test/table/table", ["two"], [2])
        hosts[1].start()

        check_replicas()

        for i in range(len(hosts)):
            actual = hosts[i].get("/test/table/table", ["one"])
            self.assertEqual(actual, row1)

            actual = hosts[i].get("/test/table/table", ["two"])
            self.assertEqual(actual, row2)

        hosts[2].stop()
        self.assertIsNone(hosts[1].delete("/test/table/table", ["one"]))
        hosts[2].start()

        check_replicas()

        for i in range(len(hosts)):
            actual = hosts[i].get("/test/table/table")
            self.assertEqual(actual, expected(SCHEMA, [["two", 2]]), f"host {i}")

        self.assertIsNone(hosts[0].delete("/test/table/truncate"))
        for i in range(len(hosts)):
            count = hosts[i].get("/test/table/table/count")
            self.assertEqual(0, count, f"host {i}")

        total = 100
        for n in range(1, total):
            i = random.choice(range(self.NUM_HOSTS))

            self.assertIsNone(hosts[i].put("/test/table/table", [num2words(n)], [n]))

            for i in range(len(hosts)):
                count = hosts[i].get("/test/table/table/count")
                self.assertEqual(n, count, f"host {i}")


class ErrorTest(unittest.TestCase):
    def setUp(self):
        class Persistent(tc.Cluster, metaclass=tc.Meta):
            __uri__ = tc.URI(f"/test/table")

            def _configure(self):
                self.table = tc.chain.Block(tc.table.Table(SCHEMA))

        self.host = start_host("table_error", [Persistent])

    def testInsert(self):
        self.assertRaises(
            tc.error.BadRequest,
            lambda: self.host.put("/test/table/table", "one", [1]))

    def tearDown(self):
        self.host.stop()


def expected(schema, rows):
    return {str(tc.uri(tc.table.Table)): [tc.to_json(schema), rows]}


if __name__ == "__main__":
    unittest.main()
