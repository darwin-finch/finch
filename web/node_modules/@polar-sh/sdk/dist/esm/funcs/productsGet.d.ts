import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Product } from "../models/components/product.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { ProductsGetRequest } from "../models/operations/productsget.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Product
 *
 * @remarks
 * Get a product by ID.
 *
 * **Scopes**: `products:read` `products:write`
 */
export declare function productsGet(client: PolarCore, request: ProductsGetRequest, options?: RequestOptions): APIPromise<Result<Product, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=productsGet.d.ts.map