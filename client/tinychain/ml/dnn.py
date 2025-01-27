# Constructors for a generic deep neural network.
#
# Prefer this implementation if no more domain-specific neural net architecture is needed.

from tinychain.collection.tensor import einsum, Dense
from tinychain.ml import Layer, NeuralNet
from tinychain.ref import After


def layer(weights, bias, activation):
    """Construct a new layer for a deep neural network."""

    # dimensions (for `einsum`): k = number of examples, i = weight input dim, j = weight output dim

    class DNNLayer(Layer):
        @property
        def bias(self):
            return Dense(self[1])

        @property
        def weights(self):
            return Dense(self[0])

        def eval(self, inputs):
            return activation.forward(einsum("ij,ki->kj", [self.weights, inputs])) + self.bias

        def gradients(self, A_prev, dA, Z):
            dZ = activation.backward(dA, Z).copy()
            dA_prev = einsum("kj,ij->ki", [dZ, self[0]])
            d_weights = einsum("kj,ki->ij", [dZ, A_prev])
            d_bias = dZ.sum(0)
            return dA_prev, d_weights, d_bias

        def train_eval(self, inputs):
            Z = einsum("ij,ki->kj", [self.weights, inputs])
            A = activation.forward(Z) + self.bias
            return A, Z

        def update(self, d_weights, d_bias):
            return self.weights.write(self.weights - d_weights), self.bias.write(self.bias - d_bias)

    return DNNLayer([weights, bias])


def neural_net(layers):
    """Construct a new deep neural network with the given layers."""

    num_layers = len(layers)

    class DNN(NeuralNet):
        def eval(self, inputs):
            state = layers[0].eval(inputs)
            for i in range(1, len(layers)):
                state = layers[i].eval(state)

            return state

        def train(self, inputs, cost):
            A = [inputs]
            Z = [None]

            for layer in layers:
                A_l, Z_l = layer.train_eval(A[-1])
                A.append(A_l.copy())
                Z.append(Z_l)

            m = inputs.shape[0]
            dA = cost(A[-1]).sum() / m

            updates = []
            for i in reversed(range(0, num_layers)):
                dA, d_weights, d_bias = layers[i].gradients(A[i], dA, Z[i + 1])
                update = layers[i].update(d_weights, d_bias)
                updates.append(update)

            return After(updates, A[-1])

    return DNN(layers)
