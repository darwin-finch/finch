import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CustomerOrderInvoice } from "../models/components/customerorderinvoice.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerPortalOrdersInvoiceRequest, CustomerPortalOrdersInvoiceSecurity } from "../models/operations/customerportalordersinvoice.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Order Invoice
 *
 * @remarks
 * Get an order's invoice data.
 *
 * **Scopes**: `customer_portal:read` `customer_portal:write`
 */
export declare function customerPortalOrdersInvoice(client: PolarCore, security: CustomerPortalOrdersInvoiceSecurity, request: CustomerPortalOrdersInvoiceRequest, options?: RequestOptions): APIPromise<Result<CustomerOrderInvoice, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerPortalOrdersInvoice.d.ts.map