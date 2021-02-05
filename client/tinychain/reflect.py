import inspect

from . import error
from .state import Class, OpRef, Scalar, State
from .util import *
from .value import Nil, Value


def gen_headers(instance):
    for name, attr in inspect.getmembers(instance):
        if name.startswith('_'):
            continue

        if isinstance(attr, MethodStub):
            setattr(instance, name, attr(instance, name))


class Meta(type):
    def __form__(cls):
        mro = cls.mro()
        parent_members = (
            {name for name, _ in inspect.getmembers(mro[1])}
            if len(mro) > 1 else set())

        class Header(cls):
            pass

        header = Header(URI("self"))
        instance = cls(URI("self"))

        for name, attr in inspect.getmembers(instance):
            if name.startswith('_') or name in parent_members:
                continue

            if isinstance(attr, MethodStub):
                setattr(header, name, attr(instance, name))
            elif isinstance(attr, State):
                setattr(header, name, type(attr)(URI(f"self/{name}")))
            else:
                setattr(header, name, attr)

        form = {}
        for name, attr in inspect.getmembers(instance):
            if name.startswith('_') or name in parent_members:
                continue

            if isinstance(attr, MethodStub):
                form[name] = to_json(attr(header, name))
            else:
                form[name] = attr

        return form

    def __json__(cls):
        return {str(uri(cls)): to_json(form_of(cls))}


class MethodStub(object):
    def __init__(self, dtype, form):
        self.dtype = dtype
        self.form = form

    def __call__(self, header, name):
        return self.dtype(header, self.form, name)


class Method(object):
    __ref__ = uri(Scalar) + "/op"

    def __init__(self, header, form, name):
        self.header = header
        self.form = form
        self.name = name

    def __json__(self):
        return {str(uri(self)): to_json(form_of(self))}


class GetMethod(Method):
    __ref__ = uri(Method) + "/get"

    def __form__(self):
        sig = inspect.signature(self.form)

        if num_args(sig) < 1 or num_args(sig) > 3:
            raise ValueError("GET method takes 1-3 arguments: (self, cxt, key)")

        args = [self.header]

        cxt = Context()
        if num_args(sig) > 1:
            args.append(cxt)

        key_name = "key"
        if num_args(sig) == 3:
            key_name, param = sig.parameters[2]
            if param.annotation in {inspect.Parameter.empty, Value}:
                args.append(Value(URI(key_name)))
            elif issubclass(param.annotation, Value):
                args.append(param.annotation(URI(key_name)))

        cxt._return = self.form(*args) # populate the Context
        return (key_name, cxt)

    def __call__(self, key=None):
        rtype = inspect.signature(self.form).return_annotation
        rtype = State if rtype == inspect.Parameter.empty else rtype
        return rtype(OpRef.Get(uri(self.header).append(self.name), key))


class PutMethod(Method):
    __def__ = uri(Method) + "/get"

    def __form__(self):
        sig = inspect.signature(self.form)

        if num_args(sig) not in [1, 2, 4]:
            raise ValueError("POST method has one, two, or four arguments: "
                + "(self, cxt, key, value)")

        args = [self.header]

        cxt = Context()
        if num_args(sig) > 1:
            args.append(cxt)

        if num_args(sig) == 4:
            name, param = sig.parameters[2]
            dtype = (Value
                if key_param.annotation == inspect.Parameter.empty
                else key_param.annotation)

            args.append(dtype(URI(name)))

            name, param = sig.parameters[3]
            dtype = (State
                if param.annotation == inspect.Parameter.empty
                else value_param.annotation)

            args.append(dtype(URI(name)))

        cxt._return = self.form(*args)
        return cxt


class PostMethod(Method):
    __def__ = uri(Method) + "/get"

    def __form__(self):
        sig = inspect.signature(self.form)

        if num_args(sig) == 0:
            raise ValueError("POST method has at least one argment: "
                + "(self, cxt, name1=val1, ...)")

        args = [self.header]
        kwargs = {}

        cxt = Context()
        if num_args(sig) > 1:
            args.append(cxt)

        for name, param in list(sig.parameters.items())[2:]:
            dtype = State if param.annotation == inspect.Parameter.empty else param.annotation
            kwargs[name] = dtype(URI(name))

        cxt._return = self.form(*args, **kwargs)
        return cxt


class DeleteMethod(Method):
    __ref__ = uri(Method) + "/get"

    def __form__(self):
        sig = inspect.signature(self.form)

        if num_args(sig) < 1 or num_args(sig) > 3:
            raise ValueError("DELETE method takes 1-3 arguments: (self, cxt, key)")

        args = [self.header]

        cxt = Context()
        if num_args(sig) > 1:
            args.append(cxt)

        key_name = "key"
        if num_args(sig) == 3:
            key_name, param = sig.parameters[2]
            if param.annotation in {inspect.Parameter.empty, Value}:
                args.append(Value(URI(key_name)))
            elif issubclass(param.annotation, Value):
                args.append(param.annotation(URI(key_name)))

        cxt._return = self.form(*args) # populate the Context
        return (key_name, cxt)


Method.Get = GetMethod
Method.Put = PutMethod
Method.Post = PostMethod
Method.Delete = DeleteMethod


def num_args(sig):
    return len(sig.parameters)
