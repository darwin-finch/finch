import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Product } from "../models/components/product.js";
import { ProductCreate } from "../models/components/productcreate.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Create Product
 *
 * @remarks
 * Create a product.
 *
 * **Scopes**: `products:write`
 */
export declare function productsCreate(client: PolarCore, request: ProductCreate, options?: RequestOptions): APIPromise<Result<Product, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=productsCreate.d.ts.map