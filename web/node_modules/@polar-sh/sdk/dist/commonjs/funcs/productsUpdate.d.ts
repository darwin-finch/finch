import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Product } from "../models/components/product.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { NotPermitted } from "../models/errors/notpermitted.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { ProductsUpdateRequest } from "../models/operations/productsupdate.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Update Product
 *
 * @remarks
 * Update a product.
 *
 * **Scopes**: `products:write`
 */
export declare function productsUpdate(client: PolarCore, request: ProductsUpdateRequest, options?: RequestOptions): APIPromise<Result<Product, NotPermitted | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=productsUpdate.d.ts.map